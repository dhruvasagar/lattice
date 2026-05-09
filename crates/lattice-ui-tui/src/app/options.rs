//! `App::resolved_option`, `OptionCache`, and per-option
//! lookup helpers -- the App-side surface above
//! `lattice-config`.
//!
//! Methods that move here in R.1:
//! - `resolved_option<D: OptionDef>(buffer_id) -> D::Value`.
//! - `set_option_global`, `set_option_buffer_local`,
//!   `set_option_window_local`.
//! - `option_invalidate_caches`,
//!   `recompute_option_caches_for_buffer`.
//! - The hot-path readers that the render loop uses
//!   (e.g., `tab_width_for`, `relative_number_for`,
//!   `colorscheme_for`).
//!
//! What does NOT live here: the option *definitions*
//! (registered via `linkme` distributed slice in
//! `lattice-config`), the type-keyed registry, the
//! TOML/init layer.
