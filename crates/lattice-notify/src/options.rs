//! NOTIF.1e: the notification subsystem's typed options.
//!
//! Owned here rather than in `lattice-config` for the reason
//! `lattice-diff`'s are — the subsystem owns its full surface, options
//! included. Self-register via `linkme`; the host's
//! `init_from_linkme()` walks the global slice at boot.

/// Validator for `notifications.max-visible`. `0` is meaningful — it
/// silences the corner entirely without unregistering the subsystem, so
/// `*messages*` still records everything. The ceiling stops a value
/// that would paper over the whole editor.
#[allow(clippy::ptr_arg)]
fn validate_max_visible(n: &i64) -> Result<(), String> {
    if *n >= 0 && *n <= 20 {
        Ok(())
    } else {
        Err(format!(
            "notifications.max-visible must be in range [0, 20], got {n}"
        ))
    }
}

/// Validator for `notifications.timeout`. Must be positive: `0` would
/// mean a notification that expires before it can be read, which is
/// indistinguishable from the invisible-completion bug the subsystem
/// exists to remove. Use `max-visible = 0` to silence the corner.
#[allow(clippy::ptr_arg)]
fn validate_timeout(n: &i64) -> Result<(), String> {
    if *n >= 1 && *n <= 3600 {
        Ok(())
    } else {
        Err(format!(
            "notifications.timeout must be in range [1, 3600] seconds, got {n} \
             (use `notifications.max-visible = 0` to show none)"
        ))
    }
}

lattice_config::options! {
    group = lattice_config::Notifications;

    /// How many notifications the corner shows at once. The rest queue,
    /// and the stack shows `+N more`.
    ///
    /// `0` shows none — the subsystem keeps running and `*messages*`
    /// keeps its record, so nothing is lost, it is just silent.
    #[name("notifications.max-visible")]
    #[validate(validate_max_visible)]
    pub NotificationsMaxVisible: i64 = 3;

    /// Seconds an **info** notification stays up.
    ///
    /// Warnings last twice this and errors four times it, and that
    /// ratio is deliberate rather than three separate knobs: an error
    /// you blink past is an error you will hit again, so raising this
    /// must not leave errors relatively shorter than the successes
    /// around them. One number keeps them ordered by construction.
    #[name("notifications.timeout")]
    #[validate(validate_timeout)]
    pub NotificationsTimeout: i64 = 4;

    /// Which corner the stack anchors to.
    #[name("notifications.corner")]
    pub NotificationsCorner: String = "bottom-right".to_string();
}
