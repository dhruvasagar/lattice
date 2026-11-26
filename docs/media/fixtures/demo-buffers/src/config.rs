/// The time one frame may take at `hz`, in milliseconds.
pub fn frame_budget(hz: u32) -> f64 {
    1000.0 / f64::from(hz)
}
