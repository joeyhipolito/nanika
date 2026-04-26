//! dust-dashboard — Ratatui TUI for the Nanika dust plugin system.

use std::io;
use std::panic;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use dust_registry::Registry;
use ratatui::{backend::CrosstermBackend, Terminal};

mod app;
mod component_renderer;
mod slash_grammar;
mod ui;

use app::App;

const POLL_INTERVAL: Duration = Duration::from_secs(1);

#[tokio::main]
async fn main() -> Result<()> {
    let original_hook = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        original_hook(info);
    }));

    // Initialise the registry BEFORE entering the alt screen so any startup
    // diagnostics (plugin spawn failures, parse errors) land on the normal
    // terminal instead of corrupting the TUI.
    let registry = Registry::new()
        .await
        .map(Arc::new)
        .context("initialise plugin registry")?;

    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("create terminal")?;

    let mut app = App::new(Arc::clone(&registry));
    app.refresh_results().await;

    let run_result = run_app(&mut terminal, &mut app).await;

    disable_raw_mode().context("disable raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
    )
    .context("leave alternate screen")?;
    terminal.show_cursor().context("show cursor")?;

    run_result
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    let mut last_poll = Instant::now();

    loop {
        terminal.draw(|f| ui::draw(f, app)).context("draw frame")?;

        let has_event = tokio::task::block_in_place(|| event::poll(Duration::from_millis(100)))
            .context("poll events")?;

        if has_event {
            let ev = tokio::task::block_in_place(event::read).context("read event")?;
            if let Event::Key(key) = ev {
                app.handle_key(key).await;
            }
        }

        // Drain any plugin events received on the live subscription channel.
        while let Some(event) = app.try_recv_event() {
            app.handle_plugin_event(event).await;
        }

        // Periodically sync the results list with the registry (hot-plug).
        if last_poll.elapsed() >= POLL_INTERVAL {
            let changed = app.poll_registry().await;
            if changed {
                // Force a full redraw to prevent ghost cells from the old list.
                terminal.clear().context("clear terminal")?;
            }
            last_poll = Instant::now();
        }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}
