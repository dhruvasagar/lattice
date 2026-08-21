//! PR.2: the `project.root-markers` option type.
//!
//! Design: [`project-resolution.md`](../../../docs/dev/architecture/project-resolution.md)
//! §4.
//!
//! The marker set `lattice_core::MarkerResolver` walks for. A list
//! option in the shape [`crate::ModelineZone`] established: TOML uses a
//! Helix-shaped array (`root-markers = [".git", "Cargo.toml"]`), `:set`
//! uses the comma form.
//!
//! **Replaces rather than extends**, and the default *is* the full
//! built-in list — so `:set project.root-markers?` shows exactly what a
//! user is about to replace. That is vim's `:set` model for list
//! options, and it is why the default is sourced from
//! [`lattice_core::DEFAULT_ROOT_MARKERS`] rather than restated here:
//! two copies of this list would drift, and the copy the resolver reads
//! is the one that matters.

use std::sync::Arc;

use crate::option_type::OptionType;

/// The ordered marker set a project root is recognised by.
///
/// Order is significant: the first marker present in a directory names
/// the resulting [`lattice_core::ProjectKind::Marker`], so `.git` ahead
/// of `Cargo.toml` means a repository that is also a crate reports
/// `.git`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RootMarkers(pub Vec<Arc<str>>);

impl Default for RootMarkers {
    fn default() -> Self {
        Self(
            lattice_core::DEFAULT_ROOT_MARKERS
                .iter()
                .map(|m| Arc::from(*m))
                .collect(),
        )
    }
}

impl RootMarkers {
    /// The markers, in priority order.
    pub fn markers(&self) -> &[Arc<str>] {
        &self.0
    }

    /// Owned `String`s, the shape `MarkerResolver::new` takes.
    pub fn to_vec(&self) -> Vec<String> {
        self.0.iter().map(|m| m.to_string()).collect()
    }
}

impl OptionType for RootMarkers {
    fn parse(s: &str) -> Result<Self, String> {
        let markers: Vec<Arc<str>> = s
            .split([',', ' ', '\t'])
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(Arc::from)
            .collect();
        // An empty set is refused rather than accepted as "no markers".
        // It would silently root every buffer at pwd — the editor would
        // still work, which is exactly what makes it a bad failure: the
        // user would see wrong roots everywhere and have no reason to
        // suspect this option.
        if markers.is_empty() {
            return Err("at least one marker is required (e.g. `.git,Cargo.toml`); \
                 an empty set would root every buffer at the working directory"
                .to_string());
        }
        Ok(RootMarkers(markers))
    }

    fn format(&self) -> String {
        self.0
            .iter()
            .map(|m| m.as_ref())
            .collect::<Vec<_>>()
            .join(",")
    }

    fn type_label() -> &'static str {
        "root-markers"
    }

    /// Open-ended: any filename can be a marker. The built-in set is
    /// offered as the completion starting point, which is also the
    /// value a user is most likely editing rather than replacing
    /// wholesale.
    fn enumerate() -> std::option::Option<Vec<&'static str>> {
        Some(lattice_core::DEFAULT_ROOT_MARKERS.to_vec())
    }

    /// A TOML array joins into this — see `loader::apply_array`.
    fn accepts_list() -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_default_is_cores_list_verbatim() {
        // Sourced, not restated: a second copy would drift from the one
        // the resolver actually walks.
        assert_eq!(
            RootMarkers::default().to_vec(),
            lattice_core::DEFAULT_ROOT_MARKERS
                .iter()
                .map(|m| m.to_string())
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn parses_the_comma_form() {
        let m = RootMarkers::parse(".git,Cargo.toml").unwrap();
        assert_eq!(m.to_vec(), vec![".git", "Cargo.toml"]);
    }

    #[test]
    fn parses_whitespace_and_tolerates_padding() {
        let m = RootMarkers::parse("  .git ,  WORKSPACE.bazel\t").unwrap();
        assert_eq!(m.to_vec(), vec![".git", "WORKSPACE.bazel"]);
    }

    #[test]
    fn an_empty_set_is_refused_with_a_reason() {
        // Accepting it would root every buffer at pwd while the editor
        // kept working — a silent wrong answer, not a visible failure.
        for input in ["", "   ", ",", " , ,\t"] {
            let err = RootMarkers::parse(input).unwrap_err();
            assert!(
                err.contains("at least one marker"),
                "input {input:?} gave {err:?}"
            );
        }
    }

    #[test]
    fn parse_round_trips_with_format() {
        // The `OptionType` contract: `T::parse(&v.format()) == Ok(v)`.
        for v in [
            RootMarkers::default(),
            RootMarkers::parse(".git").unwrap(),
            RootMarkers::parse("a,b,c").unwrap(),
        ] {
            assert_eq!(RootMarkers::parse(&v.format()).unwrap(), v);
        }
    }

    #[test]
    fn order_is_preserved_because_it_decides_the_reported_kind() {
        let m = RootMarkers::parse("Cargo.toml,.git").unwrap();
        assert_eq!(m.to_vec(), vec!["Cargo.toml", ".git"]);
    }
}
