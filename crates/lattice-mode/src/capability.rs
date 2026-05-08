//! `CapabilitySet`: typed bitfield describing what a buffer can
//! offer. Modes declare what they require via
//! [`Mode::required_capabilities`]; the registry validates the
//! buffer satisfies all required bits before activation.
//!
//! New capabilities are added as the mode system grows. Bit
//! layout is stable once a capability is shipped; a removed
//! capability leaves a hole rather than reusing the bit, so old
//! plugins compiled against the bit don't accidentally mean
//! something different.

use bitflags::bitflags;

bitflags! {
    /// Capabilities a buffer may expose to modes. A mode that
    /// requires `LSP` cannot activate on a `text-mode` buffer with
    /// no LSP attachment; the registry returns
    /// [`crate::ModeActivationError::MissingCapability`].
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
    pub struct CapabilitySet: u32 {
        /// Buffer has a stable URI (file path or URL). Most
        /// LSP-related modes need this.
        const BUFFER_URI    = 0b0000_0001;
        /// At least one LSP server is attached.
        const LSP           = 0b0000_0010;
        /// Tree-sitter parser is attached (the major mode owns
        /// the parser; minor modes that do tree-sitter queries
        /// require this).
        const TREE_SITTER   = 0b0000_0100;
        /// Buffer supports folding (rope + fold metadata).
        const FOLDS         = 0b0000_1000;
        /// Buffer is editable (not read-only). The
        /// `read-only-mode` minor flips this off.
        const WRITABLE      = 0b0001_0000;
        /// Buffer has diagnostic-overlay support (decoration
        /// provider for inline + gutter squiggles).
        const DIAGNOSTICS   = 0b0010_0000;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_satisfies_no_requirements() {
        let buf = CapabilitySet::empty();
        let req = CapabilitySet::LSP;
        assert!(!buf.contains(req));
    }

    #[test]
    fn superset_satisfies_subset() {
        let buf = CapabilitySet::BUFFER_URI | CapabilitySet::LSP | CapabilitySet::WRITABLE;
        let req = CapabilitySet::BUFFER_URI | CapabilitySet::LSP;
        assert!(buf.contains(req));
    }

    #[test]
    fn missing_bit_fails_validation() {
        let buf = CapabilitySet::BUFFER_URI | CapabilitySet::WRITABLE;
        let req = CapabilitySet::BUFFER_URI | CapabilitySet::LSP;
        assert!(!buf.contains(req));
        // The missing bits computed by `req - buf` describe what's
        // absent -- useful for the diagnostic surfaced in
        // `ModeActivationError::MissingCapability`.
        let missing = req - buf;
        assert_eq!(missing, CapabilitySet::LSP);
    }
}
