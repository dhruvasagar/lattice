//! NOTIF.1a — notifications: telling the user about work that has no
//! buffer.
//!
//! Design:
//! [`../../../docs/dev/architecture/notifications.md`], which defers the
//! specification itself to `design.md` §5.9.9 and records why the gate
//! opened. In short: `C-c g f` (fetch) fires from any buffer, returns
//! immediately with an optimistic echo, and on completion says
//! **nothing** — success is invisible and failure reaches `*messages*`
//! only. The echo area cannot fix that; it is one transient line
//! written at *fire* time, with no completion event and no persistence.
//!
//! **Three surfaces, three questions**, which is what keeps them from
//! competing: the headerline answers "what is the buffer I am looking
//! at doing?", `*messages*` answers "what happened earlier?", and a
//! notification answers "the thing I started has finished — wherever I
//! am now". A fetch has no buffer, so it is the third.
//!
//! This crate is the **data layer**: the notification, the store, and
//! expiry. Rendering is NOTIF.1b/c (both peers, one patch); magit is
//! the first consumer, NOTIF.1d.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How loudly a notification reads, and how long it lingers.
///
/// Deliberately the same three levels `EchoLevel` already carries —
/// two vocabularies for "how bad is this" would drift, and a consumer
/// mapping between them is a bug waiting to happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotificationLevel {
    #[default]
    Info,
    Warn,
    Error,
}

impl NotificationLevel {
    /// How long a notification of this level stays up by default.
    ///
    /// **Errors linger and warnings linger a little.** An error you
    /// blink past is an error you will hit again; the whole point of
    /// the subsystem is that a failed fetch stops being invisible. The
    /// numbers are defaults — a caller may override, and
    /// `notifications.default-timeout` will govern them once config
    /// lands.
    pub fn default_timeout(self) -> Duration {
        match self {
            NotificationLevel::Info => Duration::from_secs(4),
            NotificationLevel::Warn => Duration::from_secs(8),
            NotificationLevel::Error => Duration::from_secs(15),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            NotificationLevel::Info => "info",
            NotificationLevel::Warn => "warn",
            NotificationLevel::Error => "error",
        }
    }
}

/// Identity for a posted notification — the handle a consumer keeps to
/// replace or dismiss it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NotificationId(pub u64);

/// One notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub id: NotificationId,
    pub level: NotificationLevel,
    /// One line. Longer text belongs in `*messages*`, which this tees
    /// to — a corner popup that needs scrolling is the wrong surface.
    pub text: String,
    /// `None` means "stays until dismissed". Used by an operation that
    /// is still running: it posts with no timeout and *replaces* itself
    /// on completion with one that has a timeout.
    pub timeout: Option<Duration>,
}

/// The live notifications, newest last.
///
/// Window-scoped rather than buffer-scoped, and that is the point: a
/// notification exists precisely because you have moved on from
/// whatever started the work. Tying it to a buffer would hide it in the
/// buffer you are not looking at.
#[derive(Default)]
pub struct NotificationStore {
    inner: Mutex<Vec<Notification>>,
    next_id: AtomicU64,
    /// Bumped on every change, so a renderer can skip repainting the
    /// layer when nothing moved. Paramount goal #1: an idle
    /// notification must cost nothing per frame.
    version: AtomicU64,
    /// How a timed-out notification gets removed AND repainted.
    ///
    /// `None` until [`install`] wires it (a store built in a test has
    /// no editor to wake), in which case a timeout is recorded on the
    /// notification but never fires — which is what a headless store
    /// should do rather than silently spawning tasks nobody drains.
    expiry: Mutex<Option<ExpiryChannel>>,
}

/// The wake-baked sender + the runtime that sleeps on it.
///
/// **Deliberately the inbound primitive rather than a bare tick
/// callback.** An expiry is the textbook case of the bug
/// `CLAUDE.md` names: a `TickCallback` alone would remove the
/// notification and then sit there until the user happened to press a
/// key, so a popup would linger past its timeout and vanish the moment
/// you typed — which reads as a rendering bug rather than a missing
/// wake. `InboundBus::send` bakes the wake in, so it cannot be
/// forgotten here.
struct ExpiryChannel {
    bus: lattice_mode::inbound::InboundBus<NotifyInbound>,
    runtime: tokio::runtime::Handle,
}

/// What arrives on the notification subsystem's inbound bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotifyInbound {
    /// This notification's timeout elapsed.
    Expire(NotificationId),
}

pub type NotificationStoreHandle = Arc<NotificationStore>;

impl NotificationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Post a notification and return its id.
    pub fn post(&self, level: NotificationLevel, text: impl Into<String>) -> NotificationId {
        self.post_with(level, text, Some(level.default_timeout()))
    }

    /// Post with an explicit timeout — `None` for "until replaced or
    /// dismissed".
    pub fn post_with(
        &self,
        level: NotificationLevel,
        text: impl Into<String>,
        timeout: Option<Duration>,
    ) -> NotificationId {
        let id = NotificationId(self.next_id.fetch_add(1, Ordering::Relaxed));
        let notification = Notification {
            id,
            level,
            text: text.into(),
            timeout,
        };
        if let Ok(mut v) = self.inner.lock() {
            v.push(notification);
        }
        self.version.fetch_add(1, Ordering::Release);
        self.schedule_expiry(id, timeout);
        id
    }

    /// Sleep for `timeout`, then ask the editor to drop `id`.
    ///
    /// The sleep is a detached task, never a blocking wait: expiry must
    /// not hold the actor thread, and there may be several notifications
    /// counting down at once. A store with no channel (a test, a
    /// headless harness) simply does not schedule — better than
    /// spawning tasks whose sends nobody drains.
    fn schedule_expiry(&self, id: NotificationId, timeout: Option<Duration>) {
        let Some(timeout) = timeout else { return };
        let Ok(guard) = self.expiry.lock() else {
            return;
        };
        let Some(channel) = guard.as_ref() else {
            return;
        };
        let bus = channel.bus.clone();
        channel.runtime.spawn(async move {
            tokio::time::sleep(timeout).await;
            // A closed bus means the editor is gone; nothing to wake.
            let _ = bus.send(NotifyInbound::Expire(id));
        });
    }

    /// Replace `id`'s content **in place**, keeping its position in the
    /// stack.
    ///
    /// This is what a long operation uses: post "fetching…" with no
    /// timeout, then replace with "fetched" on completion. Replacing
    /// rather than dismiss-and-post keeps the row from jumping to the
    /// bottom of the stack at the moment the user looks at it — and
    /// keeps two notifications for one operation from ever being
    /// visible at once.
    ///
    /// Returns `false` if the notification is already gone (expired or
    /// dismissed), in which case the caller's completion is posted
    /// fresh by [`Self::replace_or_post`].
    pub fn replace(
        &self,
        id: NotificationId,
        level: NotificationLevel,
        text: impl Into<String>,
        timeout: Option<Duration>,
    ) -> bool {
        let Ok(mut v) = self.inner.lock() else {
            return false;
        };
        let Some(slot) = v.iter_mut().find(|n| n.id == id) else {
            return false;
        };
        slot.level = level;
        slot.text = text.into();
        slot.timeout = timeout;
        drop(v);
        self.version.fetch_add(1, Ordering::Release);
        // Re-arm: a notification posted with no timeout ("fetching…")
        // and replaced by one that has a timeout ("fetched") must
        // actually expire. Without this it would stay up forever, which
        // is the failure the no-timeout state exists to avoid in the
        // other direction.
        self.schedule_expiry(id, timeout);
        true
    }

    /// [`Self::replace`], falling back to a fresh post when the
    /// original is gone.
    ///
    /// The fallback is not defensive padding: a long fetch can outlive
    /// its own "started" notification's timeout, and silently dropping
    /// the completion would reintroduce exactly the invisible-success
    /// bug the subsystem exists to fix.
    pub fn replace_or_post(
        &self,
        id: NotificationId,
        level: NotificationLevel,
        text: impl Into<String>,
        timeout: Option<Duration>,
    ) -> NotificationId {
        let text = text.into();
        if self.replace(id, level, text.clone(), timeout) {
            id
        } else {
            self.post_with(level, text, timeout)
        }
    }

    /// Remove one notification. Idempotent — dismissing something
    /// already gone is not an error, because expiry and an explicit
    /// dismiss race by construction.
    pub fn dismiss(&self, id: NotificationId) -> bool {
        let Ok(mut v) = self.inner.lock() else {
            return false;
        };
        let before = v.len();
        v.retain(|n| n.id != id);
        let removed = v.len() != before;
        drop(v);
        if removed {
            self.version.fetch_add(1, Ordering::Release);
        }
        removed
    }

    /// Remove everything. `:notifications-clear`'s body.
    pub fn dismiss_all(&self) -> usize {
        let Ok(mut v) = self.inner.lock() else {
            return 0;
        };
        let n = v.len();
        v.clear();
        drop(v);
        if n > 0 {
            self.version.fetch_add(1, Ordering::Release);
        }
        n
    }

    /// The live notifications, oldest first.
    pub fn visible(&self) -> Vec<Notification> {
        self.inner.lock().map(|v| v.clone()).unwrap_or_default()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.lock().map(|v| v.is_empty()).unwrap_or(true)
    }

    /// Bumped on every change. A renderer that finds it unchanged has
    /// nothing to repaint.
    pub fn version(&self) -> u64 {
        self.version.load(Ordering::Acquire)
    }

    /// Give the store the channel expiry rides on. Called by
    /// [`install`]; a store without one records timeouts but never
    /// fires them.
    pub fn set_expiry_channel(
        &self,
        bus: lattice_mode::inbound::InboundBus<NotifyInbound>,
        runtime: tokio::runtime::Handle,
    ) {
        if let Ok(mut slot) = self.expiry.lock() {
            *slot = Some(ExpiryChannel { bus, runtime });
        }
    }
}

/// Wire the subsystem: register the store as a service, and give it the
/// inbound bus expiry rides on.
///
/// **The expiry path is the reason this is not just a store.** A
/// notification that vanishes only when the user next presses a key is
/// worse than one that never vanishes — it reads as a rendering bug.
/// `InboundBus::send` bakes the wake in, so the drain runs
/// off-keystroke and the layer repaints on its own.
///
/// The handler returns no `Effect`: there is nothing for the grammar to
/// apply. Removing the notification and waking the editor IS the
/// outcome, and the renderer reads the store directly.
pub fn install(boot: &mut impl lattice_mode::SubsystemBoot) {
    let store: NotificationStoreHandle = Arc::new(NotificationStore::new());
    boot.register_service::<NotificationStoreHandle>(store.clone());

    let for_handler = store.clone();
    let bus = boot.inbound::<NotifyInbound, _>(move |item| {
        match item {
            NotifyInbound::Expire(id) => {
                for_handler.dismiss(id);
            }
        }
        Vec::new()
    });
    store.set_expiry_channel(bus, boot.runtime_handle().clone());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posted_notification_is_visible_and_has_a_level_appropriate_timeout() {
        let s = NotificationStore::new();
        let id = s.post(NotificationLevel::Error, "fetch failed");
        let live = s.visible();
        assert_eq!(live.len(), 1);
        assert_eq!(live[0].id, id);
        assert_eq!(live[0].text, "fetch failed");
        assert_eq!(live[0].timeout, Some(Duration::from_secs(15)));
    }

    /// An error you blink past is an error you will hit again — the
    /// whole reason the subsystem exists is that a failed fetch stops
    /// being invisible.
    #[test]
    fn errors_linger_longer_than_info() {
        assert!(
            NotificationLevel::Error.default_timeout() > NotificationLevel::Warn.default_timeout()
        );
        assert!(
            NotificationLevel::Warn.default_timeout() > NotificationLevel::Info.default_timeout()
        );
    }

    #[test]
    fn ids_are_unique_across_posts() {
        let s = NotificationStore::new();
        let a = s.post(NotificationLevel::Info, "one");
        let b = s.post(NotificationLevel::Info, "two");
        assert_ne!(a, b);
        assert_eq!(s.visible().len(), 2);
    }

    /// The shape a long operation uses: "fetching…" with no timeout,
    /// replaced by the outcome. Replacing rather than
    /// dismiss-and-repost keeps the row from jumping to the bottom of
    /// the stack at the moment the user looks at it.
    #[test]
    fn a_replacement_keeps_its_place_in_the_stack() {
        let s = NotificationStore::new();
        let first = s.post(NotificationLevel::Info, "first");
        let running = s.post_with(NotificationLevel::Info, "fetching…", None);
        let last = s.post(NotificationLevel::Info, "last");

        assert!(s.replace(
            running,
            NotificationLevel::Info,
            "fetched",
            Some(Duration::from_secs(4))
        ));

        let live = s.visible();
        assert_eq!(
            live.iter().map(|n| n.id).collect::<Vec<_>>(),
            vec![first, running, last],
            "order is unchanged"
        );
        assert_eq!(live[1].text, "fetched");
        assert_eq!(live[1].timeout, Some(Duration::from_secs(4)));
    }

    /// A long fetch can outlive its own "started" notification.
    /// Dropping the completion there would put back exactly the
    /// invisible-success bug this subsystem exists to remove.
    #[test]
    fn a_completion_still_shows_when_its_start_already_went_away() {
        let s = NotificationStore::new();
        let running = s.post_with(NotificationLevel::Info, "fetching…", None);
        s.dismiss(running);
        assert!(s.is_empty());

        let id = s.replace_or_post(running, NotificationLevel::Info, "fetched", None);
        assert_ne!(id, running, "a fresh notification, not the dead one");
        assert_eq!(s.visible().len(), 1);
        assert_eq!(s.visible()[0].text, "fetched");
    }

    #[test]
    fn replacing_a_live_notification_reuses_its_id() {
        let s = NotificationStore::new();
        let running = s.post_with(NotificationLevel::Info, "pushing…", None);
        let id = s.replace_or_post(running, NotificationLevel::Error, "push failed", None);
        assert_eq!(id, running);
        assert_eq!(s.visible().len(), 1, "one notification, not two");
        assert_eq!(s.visible()[0].level, NotificationLevel::Error);
    }

    /// Expiry and an explicit dismiss race by construction, so
    /// dismissing something already gone must not be an error.
    #[test]
    fn dismissing_twice_is_not_an_error() {
        let s = NotificationStore::new();
        let id = s.post(NotificationLevel::Info, "x");
        assert!(s.dismiss(id));
        assert!(!s.dismiss(id));
        assert!(s.is_empty());
    }

    #[test]
    fn dismiss_all_reports_how_many_it_removed() {
        let s = NotificationStore::new();
        s.post(NotificationLevel::Info, "a");
        s.post(NotificationLevel::Warn, "b");
        assert_eq!(s.dismiss_all(), 2);
        assert_eq!(s.dismiss_all(), 0);
    }

    /// Paramount goal #1: a renderer must be able to skip the layer
    /// entirely when nothing moved, so every mutation — and only a
    /// mutation — has to bump the version.
    #[test]
    fn every_change_bumps_the_version_and_a_no_op_does_not() {
        let s = NotificationStore::new();
        let v0 = s.version();

        let id = s.post(NotificationLevel::Info, "a");
        let v1 = s.version();
        assert!(v1 > v0, "a post is a change");

        assert!(s.replace(id, NotificationLevel::Warn, "b", None));
        let v2 = s.version();
        assert!(v2 > v1, "a replace is a change");

        assert!(!s.replace(NotificationId(999), NotificationLevel::Info, "x", None));
        assert_eq!(
            s.version(),
            v2,
            "a replace that found nothing changed nothing"
        );

        assert!(!s.dismiss(NotificationId(999)));
        assert_eq!(
            s.version(),
            v2,
            "a dismiss that found nothing changed nothing"
        );

        assert_eq!(s.dismiss_all(), 1);
        assert!(s.version() > v2);

        let v3 = s.version();
        assert_eq!(s.dismiss_all(), 0);
        assert_eq!(s.version(), v3, "clearing an empty store changed nothing");
    }

    /// The expiry path, driven on a paused clock so it is
    /// deterministic rather than a sleep in the test suite.
    ///
    /// What this pins is the thing the fragment warned about: a
    /// notification must go away **on its own**, without a keystroke.
    /// The store schedules, the bus wakes, the handler dismisses.
    #[tokio::test(start_paused = true)]
    async fn a_timed_out_notification_dismisses_itself_with_no_keystroke() {
        let store: NotificationStoreHandle = Arc::new(NotificationStore::new());

        // Stand in for the host's drain: everything the bus receives is
        // applied to the store, which is exactly what `install`'s
        // handler does.
        let (bus, mut rx) = lattice_mode::inbound::make_inbound_raw::<NotifyInbound>(Arc::new(
            tokio::sync::Notify::new(),
        ));
        store.set_expiry_channel(bus, tokio::runtime::Handle::current());

        let id = store.post_with(
            NotificationLevel::Info,
            "fetched",
            Some(Duration::from_secs(4)),
        );
        assert_eq!(store.visible().len(), 1);

        // Let the spawned sleep register with the timer driver before
        // the clock moves; `post_with` is synchronous, so the task is
        // spawned but not yet polled.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        // Bounded: a broken scheduler sends nothing, and an unbounded
        // `recv().await` would hang the suite instead of failing it.
        let item = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("the expiry must arrive — nothing was scheduled")
            .expect("the bus is open");
        assert_eq!(item, NotifyInbound::Expire(id));

        store.dismiss(id);
        assert!(
            store.is_empty(),
            "the notification goes away on its own, not on the next keypress"
        );
    }

    /// A notification with no timeout is the "still running" state —
    /// it must not schedule anything, or "fetching…" would disappear
    /// mid-fetch.
    #[tokio::test(start_paused = true)]
    async fn a_notification_with_no_timeout_never_expires() {
        let store: NotificationStoreHandle = Arc::new(NotificationStore::new());
        let (bus, mut rx) = lattice_mode::inbound::make_inbound_raw::<NotifyInbound>(Arc::new(
            tokio::sync::Notify::new(),
        ));
        store.set_expiry_channel(bus, tokio::runtime::Handle::current());

        store.post_with(NotificationLevel::Info, "fetching…", None);
        tokio::time::advance(Duration::from_secs(3600)).await;

        assert!(
            rx.try_recv().is_err(),
            "nothing should have been scheduled for a timeout-less notification"
        );
        assert_eq!(store.visible().len(), 1);
    }

    /// The re-arm: "fetching…" (no timeout) replaced by "fetched"
    /// (timeout) has to start counting down, or the completion stays up
    /// forever.
    #[tokio::test(start_paused = true)]
    async fn replacing_a_timeout_less_notification_arms_its_expiry() {
        let store: NotificationStoreHandle = Arc::new(NotificationStore::new());
        let (bus, mut rx) = lattice_mode::inbound::make_inbound_raw::<NotifyInbound>(Arc::new(
            tokio::sync::Notify::new(),
        ));
        store.set_expiry_channel(bus, tokio::runtime::Handle::current());

        let id = store.post_with(NotificationLevel::Info, "fetching…", None);
        store.replace(
            id,
            NotificationLevel::Info,
            "fetched",
            Some(Duration::from_secs(4)),
        );

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), rx.recv())
                .await
                .expect("the re-armed expiry must fire")
                .expect("the bus is open"),
            NotifyInbound::Expire(id)
        );
    }

    /// A store with no channel — a test fixture, a headless harness —
    /// records the timeout but schedules nothing. Better than spawning
    /// tasks whose sends nobody drains.
    #[test]
    fn a_store_with_no_channel_records_the_timeout_and_schedules_nothing() {
        let s = NotificationStore::new();
        let id = s.post(NotificationLevel::Info, "x");
        assert_eq!(
            s.visible()[0].timeout,
            Some(NotificationLevel::Info.default_timeout())
        );
        assert!(s.visible().iter().any(|n| n.id == id));
    }

    #[test]
    fn an_empty_store_is_empty_and_versionless() {
        let s = NotificationStore::new();
        assert!(s.is_empty());
        assert!(s.visible().is_empty());
        assert_eq!(s.version(), 0);
    }
}
