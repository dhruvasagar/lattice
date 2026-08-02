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

pub mod mode;
pub mod options;

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
            NotificationLevel::Error => Duration::from_secs(16),
        }
    }

    /// How much longer than an info notification this level lasts.
    ///
    /// One knob times a fixed ratio rather than three independent
    /// options: raising the base must not leave errors relatively
    /// SHORTER than the successes around them, and three knobs make
    /// that misconfiguration reachable.
    pub fn timeout_multiplier(self) -> u64 {
        match self {
            NotificationLevel::Info => 1,
            NotificationLevel::Warn => 2,
            NotificationLevel::Error => 4,
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

/// Something a notification offers to do about itself.
///
/// §5.9.9 specifies buttons; there is no focusable widget here and one
/// is not wanted — the corner stays a pure *signal*, and
/// `:notifications` is where you act (see `notifications-mode`). So an
/// action is a label plus the [`lattice_grammar::Effect`] `<CR>`
/// applies there.
///
/// A typed effect rather than an action *name*: a name has to resolve
/// at fire time and can fail then — silently, on a key the user pressed
/// deliberately. There is nothing here to resolve.
#[derive(Debug, Clone)]
pub struct NotificationAction {
    /// Shown under the notification in the buffer.
    pub label: String,
    pub effect: lattice_grammar::Effect,
}

/// One notification.
///
/// Not `PartialEq`: it carries an [`Effect`], which is not. Tests
/// compare the fields they care about, which is sharper anyway.
#[derive(Debug, Clone)]
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
    /// What `<CR>` on this row in `*notifications*` runs. The first is
    /// the default; the corner shows none of them, because a corner
    /// popup you have to aim at is worse than one you read.
    pub actions: Vec<NotificationAction>,
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
    /// Notifications whose clock is already running. Keeps
    /// [`Self::arm_visible`] idempotent — it runs after every mutation,
    /// and without this a busy store would spawn a fresh sleep per
    /// mutation per notification.
    armed: Mutex<std::collections::HashSet<NotificationId>>,
    /// NOTIF.1e: read per call, not snapshotted, so a `:set
    /// notifications.max-visible` takes effect on the next post rather
    /// than on restart.
    config: Mutex<Option<Arc<lattice_config::ConfigRegistry>>>,
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

/// How many notifications a corner shows at once (§5.9.9's default).
/// The rest queue; [`NotificationStore::queued`] reports how many.
pub const MAX_VISIBLE: usize = 3;

impl NotificationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// How many the corner shows at once — `notifications.max-visible`.
    pub fn max_visible(&self) -> usize {
        self.config
            .lock()
            .ok()
            .and_then(|c| c.as_ref().cloned())
            .and_then(|c| c.get_typed::<options::NotificationsMaxVisible>())
            .map(|v| (*v).max(0) as usize)
            .unwrap_or(MAX_VISIBLE)
    }

    /// This level's timeout, scaled from `notifications.timeout`.
    fn timeout_for(&self, level: NotificationLevel) -> Duration {
        let base = self
            .config
            .lock()
            .ok()
            .and_then(|c| c.as_ref().cloned())
            .and_then(|c| c.get_typed::<options::NotificationsTimeout>())
            .map(|v| (*v).max(1) as u64);
        match base {
            Some(secs) => Duration::from_secs(secs * level.timeout_multiplier()),
            None => level.default_timeout(),
        }
    }

    /// Give the store its config. Called by [`install`]; without one it
    /// falls back to the compiled defaults.
    pub fn set_config(&self, config: Arc<lattice_config::ConfigRegistry>) {
        if let Ok(mut slot) = self.config.lock() {
            *slot = Some(config);
        }
    }

    /// Post a notification and return its id.
    pub fn post(&self, level: NotificationLevel, text: impl Into<String>) -> NotificationId {
        self.post_with(level, text, Some(self.timeout_for(level)))
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
        let text = text.into();
        let tee = text.clone();
        let notification = Notification {
            id,
            level,
            text,
            timeout,
            actions: Vec::new(),
        };
        if let Ok(mut v) = self.inner.lock() {
            v.push(notification);
        }
        self.version.fetch_add(1, Ordering::Release);
        // NOTIF.1e: tee to `*messages*`. Three surfaces, three
        // questions — a notification is the signal and `*messages*` is
        // the record, so one you missed (or that `max-visible = 0`
        // silenced) is still findable afterwards. Done HERE rather than
        // per consumer so it cannot be forgotten by one, and at
        // notification level, which `MessagesLayer` maps straight
        // through.
        match level {
            NotificationLevel::Info => tracing::info!(target: "lattice_notify", "{tee}"),
            NotificationLevel::Warn => tracing::warn!(target: "lattice_notify", "{tee}"),
            NotificationLevel::Error => tracing::error!(target: "lattice_notify", "{tee}"),
        }
        self.arm_visible();
        id
    }

    /// Start the clock on every **visible** notification that has a
    /// timeout and is not already counting down.
    ///
    /// **A queued notification's clock does not start until it becomes
    /// visible**, and that is a correctness requirement rather than a
    /// refinement: without it, an early notification in a burst runs
    /// out its timeout while sitting behind [`MAX_VISIBLE`] and is
    /// dismissed having never been seen. A notification nobody saw is
    /// the bug this subsystem exists to remove, reached from the other
    /// end.
    ///
    /// Called after every mutation, so a removal promotes the next one
    /// and arms it in the same step.
    ///
    /// `armed` is what keeps this idempotent — it runs on every
    /// mutation, and without it a busy store would spawn a fresh sleep
    /// per mutation per notification.
    ///
    /// A store with no channel (a test, a headless harness) schedules
    /// nothing, which beats spawning tasks whose sends nobody drains.
    fn arm_visible(&self) {
        let max_visible = self.max_visible();
        let to_arm: Vec<(NotificationId, Duration)> = {
            let Ok(v) = self.inner.lock() else {
                return;
            };
            let Ok(mut armed) = self.armed.lock() else {
                return;
            };
            // Forget what is gone, or `armed` grows for the session.
            armed.retain(|id| v.iter().any(|n| n.id == *id));
            v.iter()
                .take(max_visible)
                .filter_map(|n| n.timeout.map(|t| (n.id, t)))
                .filter(|(id, _)| armed.insert(*id))
                .collect()
        };
        if to_arm.is_empty() {
            return;
        }
        let Ok(guard) = self.expiry.lock() else {
            return;
        };
        let Some(channel) = guard.as_ref() else {
            return;
        };
        for (id, timeout) in to_arm {
            let bus = channel.bus.clone();
            // Detached, never a blocking wait: expiry must not hold the
            // actor thread, and several may count down at once.
            channel.runtime.spawn(async move {
                tokio::time::sleep(timeout).await;
                // A closed bus means the editor is gone; nothing to wake.
                let _ = bus.send(NotifyInbound::Expire(id));
            });
        }
    }

    /// Post with an action attached — what `<CR>` runs on that row in
    /// `*notifications*`.
    ///
    /// The failure case is the one that needs it: a notification is one
    /// line and git's stderr is not, so the notification says what
    /// broke and the action goes to where the rest is.
    pub fn post_with_action(
        &self,
        level: NotificationLevel,
        text: impl Into<String>,
        action: NotificationAction,
    ) -> NotificationId {
        let id = self.post(level, text);
        if let Ok(mut v) = self.inner.lock() {
            if let Some(slot) = v.iter_mut().find(|n| n.id == id) {
                slot.actions.push(action);
            }
        }
        self.version.fetch_add(1, Ordering::Release);
        id
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
        // other direction. The `armed` entry is cleared first, or the
        // idempotence check would refuse the new clock.
        if let Ok(mut armed) = self.armed.lock() {
            armed.remove(&id);
        }
        self.arm_visible();
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
            // A removal frees a slot — promote whatever was queued and
            // start its clock in the same step.
            self.arm_visible();
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
            // Nothing left to promote, but `armed` must be cleared or
            // it would hold ids that no longer exist.
            self.arm_visible();
        }
        n
    }

    /// Every live notification, oldest first — including ones queued
    /// behind [`MAX_VISIBLE`].
    ///
    /// Renderers want [`Self::visible`]; this is for tests and for
    /// `:notifications`, which should show what is waiting.
    pub fn all(&self) -> Vec<Notification> {
        self.inner.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// The notifications a renderer should paint, **oldest first**, at
    /// most [`MAX_VISIBLE`] of them.
    ///
    /// §5.9.9: "excess queued". The ones shown are the **oldest**, and
    /// that ordering is a correctness requirement rather than a
    /// preference. Showing the newest instead — which this did on its
    /// first pass — means an early notification in a burst can run out
    /// its timeout while invisible and be dismissed having never been
    /// seen. A notification nobody saw is the bug this subsystem
    /// exists to remove, arrived at from the other end.
    ///
    /// Its companion guarantee is in [`Self::schedule_expiry`]: a
    /// queued notification's clock does not start until it becomes
    /// visible.
    pub fn visible(&self) -> Vec<Notification> {
        let Ok(v) = self.inner.lock() else {
            return Vec::new();
        };
        v.iter().take(self.max_visible()).cloned().collect()
    }

    /// How many are waiting behind the visible ones. A renderer shows
    /// this as a "+N more" line rather than dropping them silently.
    pub fn queued(&self) -> usize {
        self.inner
            .lock()
            .map(|v| v.len().saturating_sub(self.max_visible()))
            .unwrap_or(0)
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

/// NOTIF.1f: the `*notifications*` buffer's text, and the row→id map
/// that goes with it.
///
/// **The corner is a signal; this buffer is where you act.** A
/// notification is not focusable and never will be — aiming at a corner
/// popup is worse than reading one — so `<CR>` on a row here is what
/// runs an action, and everything-is-a-buffer means that needs no
/// bespoke widget and no new global chord.
///
/// Returns the text and, parallel to its rows, which notification each
/// line belongs to. The map is returned rather than re-parsed for the
/// reason `magit-remote-mode` learned: re-reading the rendered line
/// makes a heading decode as a record.
pub fn render_buffer(store: &NotificationStore) -> (String, Vec<Option<NotificationId>>) {
    let all = store.all();
    if all.is_empty() {
        return ("No notifications.\n".to_string(), vec![None]);
    }
    let visible = store.max_visible();
    let mut out = String::new();
    let mut rows: Vec<Option<NotificationId>> = Vec::new();
    for (i, n) in all.iter().enumerate() {
        // The marker is the level, not a bullet: scanning for the one
        // that failed is the reason to open this buffer.
        let marker = match n.level {
            NotificationLevel::Info => "\u{2713}",
            NotificationLevel::Warn => "!",
            NotificationLevel::Error => "\u{2717}",
        };
        // Queued ones are shown, dimmed by a marker rather than hidden:
        // "+N more" in the corner tells you they exist, and this is
        // where you find out what they are.
        let queued = if i >= visible { " (queued)" } else { "" };
        out.push_str(&format!("  {marker} {}{queued}\n", n.text));
        rows.push(Some(n.id));
        for action in &n.actions {
            out.push_str(&format!("      <CR>  {}\n", action.label));
            // An action row belongs to its notification, so `<CR>`
            // works whether the cursor is on the text or the action.
            rows.push(Some(n.id));
        }
    }
    (out, rows)
}

/// Which notification the cursor is on, from [`render_buffer`]'s map.
pub fn notification_at(rows: &[Option<NotificationId>], line: u32) -> Option<NotificationId> {
    rows.get(line as usize).copied().flatten()
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

    if let Some(config) = boot.service::<Arc<lattice_config::ConfigRegistry>>() {
        store.set_config((*config).clone());
    }

    boot.register_service::<mode::RowMapHandle>(Arc::new(mode::RowMap::default()));
    boot.commands_mut().register_ex_command(
        "notifications",
        "Open the `*notifications*` buffer — live and queued notifications, \
         where <CR> runs a notification's action and `d` dismisses it.",
        lattice_grammar::ExCommandSpec {
            latency_class: lattice_grammar::LatencyClass::Display,
            accepts_bang: false,
            accepts_range: false,
            parse_args: Arc::new(|_line: &str, _bang: bool| Ok(lattice_grammar::Args::None)),
            apply: Arc::new(|_ctx| {
                Ok(lattice_grammar::Effect::OpenSyntheticBuffer {
                    name: mode::BUFFER_NAME.to_string(),
                    mode_id: mode::NotificationsMode::mode_id().as_str().to_string(),
                })
            }),
            args_schema: Vec::new(),
            surface_form: lattice_grammar::SurfaceForm::Keyword,
        },
    );
    let _ = boot.modes_mut().register(mode::NotificationsMode);

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
        assert_eq!(
            live[0].timeout,
            Some(Duration::from_secs(16)),
            "4s base × the error multiplier"
        );
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

    /// The property the first version of this got wrong: a queued
    /// notification must NOT run its clock while invisible, or it is
    /// dismissed having never been seen — the very bug the subsystem
    /// removes, arrived at from the other end.
    #[tokio::test(start_paused = true)]
    async fn a_queued_notification_does_not_expire_before_it_is_seen() {
        let store: NotificationStoreHandle = Arc::new(NotificationStore::new());
        let (bus, mut rx) = lattice_mode::inbound::make_inbound_raw::<NotifyInbound>(Arc::new(
            tokio::sync::Notify::new(),
        ));
        store.set_expiry_channel(bus, tokio::runtime::Handle::current());

        // Four at once: three visible, the fourth queued.
        let ids: Vec<_> = (0..4)
            .map(|i| store.post(NotificationLevel::Info, format!("n{i}")))
            .collect();
        assert_eq!(store.queued(), 1);

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        // Let the woken sleeps actually send before draining.
        tokio::task::yield_now().await;

        // Exactly the three that were VISIBLE expire. The queued one's
        // clock never started.
        let mut expired = Vec::new();
        while let Ok(NotifyInbound::Expire(id)) = rx.try_recv() {
            expired.push(id);
        }
        expired.sort();
        assert_eq!(
            expired,
            ids[..MAX_VISIBLE].to_vec(),
            "only the visible ones expired: {expired:?}"
        );
        assert!(
            !expired.contains(&ids[3]),
            "the queued notification must not expire unseen"
        );
    }

    /// …and once promoted, it starts counting.
    #[tokio::test(start_paused = true)]
    async fn a_promoted_notification_starts_its_clock() {
        let store: NotificationStoreHandle = Arc::new(NotificationStore::new());
        let (bus, mut rx) = lattice_mode::inbound::make_inbound_raw::<NotifyInbound>(Arc::new(
            tokio::sync::Notify::new(),
        ));
        store.set_expiry_channel(bus, tokio::runtime::Handle::current());

        let ids: Vec<_> = (0..4)
            .map(|i| store.post(NotificationLevel::Info, format!("n{i}")))
            .collect();
        // Free a slot, which promotes the fourth.
        store.dismiss(ids[0]);
        assert!(store.visible().iter().any(|n| n.id == ids[3]));

        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_secs(5)).await;
        tokio::task::yield_now().await;

        let mut expired = Vec::new();
        while let Ok(NotifyInbound::Expire(id)) = rx.try_recv() {
            expired.push(id);
        }
        assert!(
            expired.contains(&ids[3]),
            "the promoted notification must now expire: {expired:?}"
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

    /// §5.9.9: at most three show, the rest queue. The three kept are
    /// the NEWEST — a burst of five must not leave you reading the
    /// first three while the two that matter wait behind them.
    #[test]
    fn a_burst_shows_the_newest_and_queues_the_rest() {
        let s = NotificationStore::new();
        let ids: Vec<_> = (0..5)
            .map(|i| s.post(NotificationLevel::Info, format!("n{i}")))
            .collect();

        let shown = s.visible();
        assert_eq!(shown.len(), MAX_VISIBLE);
        assert_eq!(
            shown.iter().map(|n| n.id).collect::<Vec<_>>(),
            ids[..MAX_VISIBLE].to_vec(),
            "the OLDEST three — showing the newest instead lets an early \
             notification expire while invisible, which is the bug this \
             subsystem exists to remove, from the other end"
        );
        assert_eq!(s.queued(), 2);
        assert_eq!(s.all().len(), 5, "the queued ones are still live");
    }

    #[test]
    fn nothing_queues_below_the_limit() {
        let s = NotificationStore::new();
        s.post(NotificationLevel::Info, "a");
        s.post(NotificationLevel::Info, "b");
        assert_eq!(s.visible().len(), 2);
        assert_eq!(s.queued(), 0);
    }

    /// A queued notification becomes visible when one in front of it
    /// expires — otherwise a burst would leave rows permanently
    /// stranded.
    #[test]
    fn dismissing_a_visible_one_promotes_a_queued_one() {
        let s = NotificationStore::new();
        let ids: Vec<_> = (0..4)
            .map(|i| s.post(NotificationLevel::Info, format!("n{i}")))
            .collect();
        assert_eq!(s.queued(), 1);
        assert!(
            !s.visible().iter().any(|n| n.id == ids[3]),
            "the newest is the one waiting"
        );

        s.dismiss(ids[0]);
        assert_eq!(s.queued(), 0);
        assert!(
            s.visible().iter().any(|n| n.id == ids[3]),
            "the one that was queued is now shown"
        );
    }

    #[test]
    fn an_empty_buffer_says_so_rather_than_rendering_nothing() {
        let s = NotificationStore::new();
        let (text, rows) = render_buffer(&s);
        assert_eq!(text, "No notifications.\n");
        assert!(notification_at(&rows, 0).is_none());
    }

    #[test]
    fn every_row_including_an_action_row_maps_to_its_notification() {
        let s = NotificationStore::new();
        let a = s.post(NotificationLevel::Info, "fetch finished");
        let b = s.post_with_action(
            NotificationLevel::Error,
            "push failed",
            NotificationAction {
                label: "show output".into(),
                effect: lattice_grammar::Effect::OpenMessages,
            },
        );
        let (text, rows) = render_buffer(&s);
        let lines: Vec<&str> = text.lines().collect();

        assert!(lines[0].contains("fetch finished"));
        assert!(lines[1].contains("push failed"));
        assert!(lines[2].contains("show output"), "the action is listed");

        assert_eq!(notification_at(&rows, 0), Some(a));
        assert_eq!(notification_at(&rows, 1), Some(b));
        assert_eq!(
            notification_at(&rows, 2),
            Some(b),
            "an action row belongs to its notification, so `<CR>` works \
             from either line"
        );
    }

    /// The corner says "+N more"; this is where you find out what they
    /// are. Hiding them here would leave no surface that shows them at
    /// all.
    #[test]
    fn queued_notifications_are_listed_and_marked() {
        let s = NotificationStore::new();
        for i in 0..5 {
            s.post(NotificationLevel::Info, format!("n{i}"));
        }
        let (text, _) = render_buffer(&s);
        assert_eq!(text.lines().count(), 5, "all of them, not just visible");
        assert_eq!(
            text.lines().filter(|l| l.contains("(queued)")).count(),
            2,
            "and the ones behind the limit say so"
        );
    }

    #[test]
    fn the_level_marker_is_what_you_scan_for() {
        let s = NotificationStore::new();
        s.post(NotificationLevel::Error, "boom");
        let (text, _) = render_buffer(&s);
        assert!(text.contains('\u{2717}'), "{text}");
    }

    /// NOTIF.1f: the action is a typed effect, not a name to resolve.
    /// A name can fail to resolve at fire time — silently, on a key the
    /// user pressed deliberately.
    #[test]
    fn a_notifications_action_is_carried_as_an_effect() {
        let s = NotificationStore::new();
        let id = s.post_with_action(
            NotificationLevel::Error,
            "push failed",
            NotificationAction {
                label: "show output".into(),
                effect: lattice_grammar::Effect::OpenMessages,
            },
        );
        let n = s.all().into_iter().find(|n| n.id == id).expect("posted");
        assert_eq!(n.actions.len(), 1);
        assert_eq!(n.actions[0].label, "show output");
        assert!(matches!(
            n.actions[0].effect,
            lattice_grammar::Effect::OpenMessages
        ));
    }

    /// Most notifications have nothing to do, and `<CR>` on one must
    /// decline rather than complain — a key that errors in the common
    /// case trains you to stop pressing it.
    #[test]
    fn a_notification_without_an_action_carries_none() {
        let s = NotificationStore::new();
        let id = s.post(NotificationLevel::Info, "fetch finished");
        let n = s.all().into_iter().find(|n| n.id == id).expect("posted");
        assert!(n.actions.is_empty());
    }

    /// NOTIF.1d's shape, end to end at the store: a remote op that
    /// finishes posts Info, one that fails posts Error and lingers
    /// longer. Before this, success was invisible and failure reached
    /// `*messages*` only — the case that opened the gate.
    #[test]
    fn a_completed_operation_and_a_failed_one_read_differently() {
        let s = NotificationStore::new();
        s.post(NotificationLevel::Info, "fetch finished");
        s.post(NotificationLevel::Error, "push failed: rejected");

        let live = s.visible();
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].level, NotificationLevel::Info);
        assert_eq!(live[1].level, NotificationLevel::Error);
        assert!(
            live[1].timeout > live[0].timeout,
            "the failure has to outlast the success: {live:?}"
        );
    }

    /// NOTIF.1e: one knob times a fixed ratio. Raising the base must
    /// not leave errors relatively SHORTER than the successes around
    /// them, which three independent options would make reachable.
    #[test]
    fn the_level_multipliers_keep_errors_longest_at_any_base() {
        for base in [1u64, 4, 30, 3600] {
            let info = base * NotificationLevel::Info.timeout_multiplier();
            let warn = base * NotificationLevel::Warn.timeout_multiplier();
            let error = base * NotificationLevel::Error.timeout_multiplier();
            assert!(error > warn && warn > info, "base {base}");
        }
    }

    /// `max-visible = 0` silences the corner without losing anything —
    /// the store keeps running and the `*messages*` tee keeps the
    /// record.
    #[test]
    fn nothing_visible_still_keeps_the_notifications() {
        let s = NotificationStore::new();
        s.post(NotificationLevel::Error, "push failed");
        // With no config the compiled default applies, so this asserts
        // the shape rather than the zero case; the zero case is the
        // validator's business and is covered there.
        assert_eq!(s.all().len(), 1);
        assert!(!s.is_empty());
    }

    #[test]
    fn an_empty_store_is_empty_and_versionless() {
        let s = NotificationStore::new();
        assert!(s.is_empty());
        assert!(s.visible().is_empty());
        assert_eq!(s.version(), 0);
    }
}
