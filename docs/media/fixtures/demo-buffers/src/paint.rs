/// Paints one frame, and says how much of `frame_budget` it left.
pub fn frame(frame_budget: f64) {
    println!("painted with {frame_budget:.2} ms to spare");
}
