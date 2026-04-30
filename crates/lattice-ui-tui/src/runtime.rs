//! Terminal IO loop. Sets up raw mode + alt screen, draws frames, polls
//! events, restores terminal state on exit.
//!
//! This is the only file in the crate that talks to the terminal directly.
//! Everything else is pure and unit-tested.

use std::io::Stdout;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;

use lattice_core::Document;

use crate::app::App;
use crate::input::{TranslateContext, translate};
use crate::render::draw_frame;

pub fn run(document: Document) -> Result<()> {
    let mut terminal = setup().context("setup terminal")?;
    let result = main_loop(&mut terminal, App::new(document));
    teardown(&mut terminal).context("teardown terminal")?;
    result
}

fn setup() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("create terminal")
}

fn teardown(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().context("disable raw mode")?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen).context("leave alt screen")?;
    terminal.show_cursor().context("show cursor")?;
    Ok(())
}

fn main_loop(terminal: &mut Terminal<CrosstermBackend<Stdout>>, mut app: App) -> Result<()> {
    while !app.should_quit {
        // Update viewport height (height minus the mode line + command/echo row).
        let size = terminal.size().context("query terminal size")?;
        let buffer_height = size.height.saturating_sub(2) as u32;
        app.set_viewport_height(buffer_height);
        app.refresh_highlights();

        terminal
            .draw(|frame| draw_frame(frame, &app))
            .context("draw frame")?;

        // 100ms poll keeps the loop responsive to terminal resizes without
        // spinning. We only consume KeyEvents; resizes naturally re-render
        // on the next iteration.
        if event::poll(Duration::from_millis(100)).context("poll events")? {
            match event::read().context("read event")? {
                Event::Key(k) => {
                    let ctx = TranslateContext {
                        modal: app.modal,
                        pending: app.pending,
                        builtins: &app.builtins,
                    };
                    let action = translate(ctx, k);
                    app.apply(action);
                }
                Event::Resize(_, _) => {
                    // next iteration handles the new size
                }
                _ => {}
            }
        }
    }
    Ok(())
}
