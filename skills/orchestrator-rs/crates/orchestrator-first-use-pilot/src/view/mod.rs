//! Read-only terminal mission inspector. Device handling, retained state, and
//! rendering are deliberately separate so the latter two stay pure-testable.

mod model;
mod render;
mod terminal;

use std::io::{self, Write};
use std::time::Duration;

use signal_hook::consts::signal::{SIGINT, SIGTERM};
use signal_hook::iterator::Signals;
use thiserror::Error;

use crate::PilotOptions;
use crate::observe::{ObservationFeed, ObserveError, safe_text};
use model::{Focus, ViewModel};
use terminal::{Key, TerminalGuard};

const INPUT_WAIT: Duration = Duration::from_millis(50);
const MAX_READS_PER_TICK: usize = 8;
const DETAIL_SCROLL_STEP: usize = 8;

#[derive(Debug, Error)]
pub(crate) enum ViewError {
    #[error(transparent)]
    Source(#[from] ObserveError),
    #[error("terminal: {0}")]
    Terminal(#[from] io::Error),
    #[error("installing viewer signal handling: {0}")]
    Signals(io::Error),
    #[error("view lost its required progress-log path")]
    MissingSource,
}

pub(crate) fn run(options: &PilotOptions, output: &mut impl Write) -> Result<(), ViewError> {
    let path = options
        .progress_log
        .as_deref()
        .ok_or(ViewError::MissingSource)?;
    let mut signals = Signals::new([SIGINT, SIGTERM]).map_err(ViewError::Signals)?;
    let mut terminal = TerminalGuard::enter(output)?;
    let mut feed = ObservationFeed::open(path)?;
    let mut input = terminal::Input::default();
    let source = safe_text(&path.display().to_string());
    let mut model = ViewModel::new(options.observe_follow);

    loop {
        if signals.pending().next().is_some() {
            break;
        }
        if model.replaying || model.following {
            for _ in 0..MAX_READS_PER_TICK {
                match feed.step() {
                    Ok(step) => {
                        for observation in step.observations {
                            model.ingest(observation);
                        }
                        if step.at_eof {
                            if model.replaying && !model.following {
                                for observation in feed.finish_replay()? {
                                    model.ingest(observation);
                                }
                                model.replay_complete = true;
                                model.following = false;
                            }
                            model.replaying = false;
                            if model.following {
                                match feed.source_still_matches() {
                                    Ok(true) => {}
                                    Ok(false) => {
                                        model.source_error =
                                            Some("progress log was replaced or truncated".into());
                                        model.following = false;
                                    }
                                    Err(error) => {
                                        model.source_error = Some(safe_text(&error.to_string()));
                                        model.following = false;
                                    }
                                }
                            }
                            break;
                        }
                    }
                    Err(error) => {
                        model.source_error = Some(safe_text(&error.to_string()));
                        model.following = false;
                        model.replaying = false;
                        break;
                    }
                }
            }
        }

        let (width, height) = terminal::dimensions();
        write!(
            terminal,
            "{}",
            render::screen(&model, &source, width, height)
        )?;
        terminal.flush()?;

        if let Some(key) = input.read_key(INPUT_WAIT)? {
            if handle_key(&mut model, key) {
                break;
            }
        }
    }
    Ok(())
}

fn handle_key(model: &mut ViewModel, key: Key) -> bool {
    if model.editing_search {
        match key {
            Key::Enter | Key::Escape => model.editing_search = false,
            Key::Backspace => model.pop_search(),
            Key::Interrupt => return true,
            Key::Character(character) => model.push_search(character),
            _ => {}
        }
        return false;
    }
    match key {
        Key::Character('q') | Key::Interrupt => return true,
        Key::Character('j') | Key::Down => model.move_selection(1),
        Key::Character('k') | Key::Up => model.move_selection(-1),
        Key::Tab => {
            model.focus = match model.focus {
                Focus::Phases => Focus::Activity,
                Focus::Activity => Focus::Phases,
            };
        }
        Key::Enter => {
            model.detail_open = !model.detail_open;
            model.detail_scroll = 0;
        }
        Key::Character('/') => model.editing_search = true,
        Key::Character('t') => model.set_filter(model.filter.next()),
        Key::Character('f') => {
            if model.source_error.is_none() {
                if model.replaying {
                    model.replaying = false;
                    model.following = false;
                } else {
                    model.following = !model.following;
                    model.replay_complete = false;
                    if model.following {
                        model.select_last();
                    }
                }
            }
        }
        Key::PageUp if model.detail_open => {
            model.detail_scroll = model.detail_scroll.saturating_sub(DETAIL_SCROLL_STEP);
        }
        Key::PageDown if model.detail_open => {
            model.detail_scroll = model.detail_scroll.saturating_add(DETAIL_SCROLL_STEP);
        }
        _ => {}
    }
    false
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::json;

    use super::*;

    #[test]
    fn q_detaches_but_search_q_edits_query() {
        let mut model = ViewModel::new(false);
        model.editing_search = true;
        assert!(!handle_key(&mut model, Key::Character('q')));
        assert_eq!(model.search, "q");
        model.editing_search = false;
        assert!(handle_key(&mut model, Key::Character('q')));
    }

    #[test]
    fn view_arguments_and_help_describe_the_read_only_command()
    -> Result<(), Box<dyn std::error::Error>> {
        let options =
            crate::parse_arguments(["view", "--progress-log", "saved.jsonl", "--follow"])?;
        assert_eq!(options.command, crate::PilotCommand::View);
        let mut output = Vec::new();
        let mut errors = Vec::new();
        assert_eq!(
            crate::main_with(["view", "--help"], None, &mut output, &mut errors),
            0
        );
        let help = String::from_utf8(output)?;
        assert!(help.contains("orchestrator-first-use-pilot view"));
        assert!(help.contains("--progress-log <regular-file> [--follow]"));
        Ok(())
    }

    #[test]
    fn actual_nested_saved_events_form_one_command_card() -> Result<(), Box<dyn std::error::Error>>
    {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nanika-view-feed-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let nested = [
            json!({"type":"thread.started","thread_id":"session-a"}),
            json!({"type":"item.started","item":{"id":"command-a","type":"command_execution","command":"printf hello"}}),
            json!({"type":"item.completed","item":{"id":"command-a","type":"command_execution","command":"printf hello","aggregated_output":"hello","exit_code":0,"status":"completed"}}),
        ]
        .iter()
        .map(|value| format!("{value}\n"))
        .collect::<String>();
        let wrapper = json!({"schema":"nanika.rust-pilot.progress.v1","kind":"process_output",
            "phase_id":"code","stream":"stdout","text":nested});
        fs::write(&path, format!("{wrapper}\n"))?;
        let mut feed = ObservationFeed::open(&path)?;
        let mut model = ViewModel::new(false);
        loop {
            let step = feed.step()?;
            for observation in step.observations {
                model.ingest(observation);
            }
            if step.at_eof {
                break;
            }
        }
        let tools: Vec<_> = model.visible().into_iter().filter(|row| row.tool).collect();
        fs::remove_file(path)?;
        assert_eq!(tools.len(), 1);
        assert!(!tools[0].unresolved);
        assert!(tools[0].detail.contains("aggregated_output"));
        Ok(())
    }

    #[test]
    fn supervisor_failure_does_not_turn_started_owner_phase_live() {
        let mut model = ViewModel::new(false);
        model.ingest(
            json!({"sequence":1,"kind":"owner_journal","phase_id":"code",
            "body":{"detail":{"status":"started"}}}),
        );
        model.ingest(json!({"sequence":2,"kind":"supervisor_error",
            "body":{"reason":"invocation reported failure"}}));
        assert!(!model.following);
        assert!(model.visible().iter().any(|row| row.problem));
    }
}
