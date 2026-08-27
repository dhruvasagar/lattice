//! WT.1: the `wit/` embedding moved to the `lattice-wit` crate, which is what a
//! PLUGIN depends on as well — one copy of the API package rather than one for
//! the scaffold and one for everyone else. This build script is retained only
//! for the version stamp below; the WIT itself comes from the dependency.

fn main() {
    // Nothing to generate. Kept as a file so `cargo:rerun-if-changed` hints
    // can be added here without resurrecting a build script.
}
