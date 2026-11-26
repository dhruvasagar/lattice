mod config;
mod paint;

fn main() {
    let frame_budget = config::frame_budget(120);
    paint::frame(frame_budget);
}
