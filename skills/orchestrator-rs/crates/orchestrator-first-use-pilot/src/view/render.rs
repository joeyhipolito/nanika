use super::model::{Activity, Focus, ViewModel};
use crate::observe::safe_text;
use unicode_width::UnicodeWidthStr;

const RESET: &str = "\x1b[0m";
const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const SELECTED: &str = "\x1b[7m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";

pub(crate) fn screen(model: &ViewModel, source: &str, width: usize, height: usize) -> String {
    let width = width.clamp(1, 300);
    let height = height.clamp(1, 120);
    if width < 20 || height < 6 {
        return format!("\x1b[H\x1b[J{}", fit("Pane too small; q detaches", width));
    }
    let mut lines = Vec::with_capacity(height);
    let state = if model.source_error.is_some() {
        "SOURCE ERROR"
    } else if model.replaying {
        "REPLAYING"
    } else if model.following {
        "FOLLOWING LOG"
    } else if model.replay_complete {
        "REPLAY COMPLETE"
    } else {
        "PAUSED / REPLAY"
    };
    lines.push(fit(
        &format!(
            "{BOLD}Mission inspector{RESET}  {CYAN}{state}{RESET}  {}",
            compact_path(source)
        ),
        width,
    ));
    let lost = model
        .evicted_records
        .saturating_add(model.evicted_tools)
        .saturating_add(model.omitted_phase_observations);
    let search = if model.editing_search {
        format!(" /{}▌", model.search)
    } else if model.search.is_empty() {
        String::new()
    } else {
        format!(" /{}", model.search)
    };
    lines.push(fit(
        &format!(
            "filter:{}{}  shown:{}/retained:{}  evicted/lost:{}  usage:{}  deps:{}",
            model.filter.label(),
            search,
            model.visible().len(),
            model.retained_count(),
            lost,
            if model.usage_observed {
                "observed"
            } else {
                "unavailable"
            },
            if model.dependencies_observed {
                "observed"
            } else {
                "unavailable"
            }
        ),
        width,
    ));

    let content_height = height.saturating_sub(4);
    if width >= 90 {
        render_wide(model, width, content_height, &mut lines);
    } else {
        render_compact(model, width, content_height, &mut lines);
    }
    let owner = model.owner_status();
    let provider = if model.source_error.is_some() {
        "source error"
    } else {
        model.provider_status()
    };
    lines.push(fit(&format!("owner: {owner}  provider: {provider}"), width));
    lines.push(fit(
        "j/k ↑/↓ select  Tab focus  Enter details  PgUp/PgDn scroll  / search  t filter  f follow/pause  q detach",
        width,
    ));
    if let Some(error) = &model.source_error {
        if height > 6 {
            let index = height.saturating_sub(3);
            if index < lines.len() {
                lines[index] = fit(&format!("{RED}source error: {error}{RESET}"), width);
            }
        }
    }
    while lines.len() < height {
        lines.insert(lines.len().saturating_sub(2), String::new());
    }
    lines.truncate(height);
    format!(
        "\x1b[H{}",
        lines
            .into_iter()
            .map(|line| format!("{line}\x1b[K"))
            .collect::<Vec<_>>()
            .join("\r\n")
    )
}

fn render_wide(model: &ViewModel, width: usize, height: usize, lines: &mut Vec<String>) {
    let phase_width = 22.min(width / 4);
    let remaining = width.saturating_sub(phase_width + 2);
    let activity_width = if model.detail_open {
        remaining / 2
    } else {
        remaining
    };
    let detail_width = remaining.saturating_sub(activity_width + usize::from(model.detail_open));
    let phases = phase_lines(model, height, phase_width);
    let activity = activity_lines(model, height, activity_width);
    let detail = if model.detail_open {
        detail_lines(model, height, detail_width)
    } else {
        Vec::new()
    };
    for row in 0..height {
        let mut line = format!(
            "{}│{}",
            pad(
                phases.get(row).map(String::as_str).unwrap_or(""),
                phase_width
            ),
            pad(
                activity.get(row).map(String::as_str).unwrap_or(""),
                activity_width
            )
        );
        if model.detail_open {
            line.push('│');
            line.push_str(&pad(
                detail.get(row).map(String::as_str).unwrap_or(""),
                detail_width,
            ));
        }
        lines.push(fit(&line, width));
    }
}

fn render_compact(model: &ViewModel, width: usize, height: usize, lines: &mut Vec<String>) {
    let phase = if model.selected_phase == 0 {
        "all phases".to_owned()
    } else {
        model
            .phases()
            .get(model.selected_phase - 1)
            .map(|phase| {
                format!(
                    "phase {} · {}",
                    phase.id,
                    phase.route.as_deref().unwrap_or("route unavailable")
                )
            })
            .unwrap_or_else(|| "all phases".into())
    };
    lines.push(fit(&format!("{BOLD}{phase}{RESET}"), width));
    let body_height = height.saturating_sub(1);
    let body = if model.detail_open {
        detail_lines(model, body_height, width)
    } else if model.focus == Focus::Phases {
        phase_lines(model, body_height, width)
    } else {
        activity_lines(model, body_height, width)
    };
    lines.extend(body);
    while lines.len() < height.saturating_add(2) {
        lines.push(String::new());
    }
}

fn phase_lines(model: &ViewModel, height: usize, width: usize) -> Vec<String> {
    let mut rows = Vec::new();
    rows.push(selectable(
        "All phases",
        model.selected_phase == 0 && model.focus == Focus::Phases,
    ));
    for (index, phase) in model.phases().iter().enumerate() {
        let text = format!(
            "{} {}",
            phase.id,
            phase.route.as_deref().unwrap_or("route unavailable")
        );
        rows.push(selectable(
            &text,
            model.selected_phase == index + 1 && model.focus == Focus::Phases,
        ));
    }
    visible_window(rows, model.selected_phase, height, width)
}

fn activity_lines(model: &ViewModel, height: usize, width: usize) -> Vec<String> {
    let visible = model.visible();
    if visible.is_empty() {
        return vec![fit(&format!("{DIM}No matching activity{RESET}"), width)];
    }
    let selected = model.selected().map(|row| row.sequence);
    let selected_index = selected
        .and_then(|sequence| visible.iter().position(|row| row.sequence == sequence))
        .unwrap_or(0);
    let rows: Vec<String> = visible
        .iter()
        .map(|row| {
            activity_row(
                row,
                selected == Some(row.sequence) && model.focus == Focus::Activity,
            )
        })
        .collect();
    visible_window(rows, selected_index, height, width)
}

fn activity_row(row: &Activity, selected: bool) -> String {
    let marker = if row.problem {
        "!"
    } else if row.unresolved {
        "?"
    } else {
        "·"
    };
    let phase = row.phase.as_deref().unwrap_or("phase unavailable");
    let session = row.session.as_deref().unwrap_or("session unavailable");
    selectable(
        &format!(
            "{marker} {}  [{} · {}] #{:06}",
            row.summary, phase, session, row.sequence
        ),
        selected,
    )
}

fn detail_lines(model: &ViewModel, height: usize, width: usize) -> Vec<String> {
    let Some(selected) = model.selected() else {
        return vec![fit("Detail unavailable: no selected row", width)];
    };
    let heading = format!(
        "{} #{}{}",
        selected.kind,
        selected.sequence,
        if selected.complete {
            ""
        } else {
            " · truncated"
        }
    );
    let mut rows = vec![format!("{BOLD}{heading}{RESET}")];
    if selected.unresolved {
        rows.push("result: unavailable (no matching result observed)".into());
    }
    for line in selected.detail.lines() {
        rows.extend(wrap(line, width));
    }
    rows.into_iter()
        .skip(model.detail_scroll)
        .take(height)
        .map(|line| fit(&line, width))
        .collect()
}

fn visible_window(rows: Vec<String>, selected: usize, height: usize, width: usize) -> Vec<String> {
    let start = selected.saturating_sub(height.saturating_sub(1));
    rows.into_iter()
        .skip(start)
        .take(height)
        .map(|row| fit(&row, width))
        .collect()
}

fn selectable(text: &str, selected: bool) -> String {
    if selected {
        format!("{SELECTED}{text}{RESET}")
    } else {
        text.to_owned()
    }
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut rows = Vec::new();
    let mut line = String::new();
    for character in text.chars() {
        line.push(character);
        if line.width() > width {
            line.pop();
            if !line.is_empty() {
                rows.push(std::mem::take(&mut line));
            }
            line.push(character);
            if line.width() > width {
                line.clear();
                line.push('?');
            }
        }
    }
    if !line.is_empty() || rows.is_empty() {
        rows.push(line);
    }
    rows
}

fn fit(text: &str, width: usize) -> String {
    let truncated = display_width(text) > width;
    let limit = if truncated {
        width.saturating_sub(1)
    } else {
        width
    };
    let mut escape = false;
    let mut plain = String::new();
    let mut result = String::new();
    for character in text.chars() {
        if character == '\x1b' {
            escape = true;
            result.push(character);
            continue;
        }
        if escape {
            result.push(character);
            if character == 'm' {
                escape = false;
            }
            continue;
        }
        plain.push(character);
        if plain.width() > limit {
            break;
        }
        result.push(character);
    }
    if truncated && width > 0 {
        result.push('…');
    }
    result.push_str(RESET);
    result
}

fn pad(text: &str, width: usize) -> String {
    let fitted = fit(text, width);
    let visible = display_width(&fitted);
    format!("{fitted}{}", " ".repeat(width.saturating_sub(visible)))
}

fn display_width(text: &str) -> usize {
    let mut escape = false;
    let plain: String = text
        .chars()
        .filter(|character| {
            if *character == '\x1b' {
                escape = true;
                return false;
            }
            if escape {
                if *character == 'm' {
                    escape = false;
                }
                return false;
            }
            !character.is_control()
        })
        .collect();
    plain.width()
}

fn compact_path(path: &str) -> String {
    let path = safe_text(path);
    if path.chars().count() <= 80 {
        path
    } else {
        let tail: String = path
            .chars()
            .rev()
            .take(76)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        format!("…/{tail}")
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn wide_and_combining_text_obeys_terminal_cell_budgets() {
        for text in ["漢字漢字", "🙂🙂", "❤️❤️", "éé", "👩‍💻👩‍💻"]
        {
            for width in 1..12 {
                assert!(display_width(&fit(text, width)) <= width);
                assert_eq!(display_width(&pad(text, width)), width);
                assert!(wrap(text, width).iter().all(|line| line.width() <= width));
            }
        }
        assert!(screen(&ViewModel::new(false), "", 8, 2).lines().count() <= 2);
    }

    #[test]
    fn render_is_dimension_bounded_and_escapes_source_controls() {
        let mut model = ViewModel::new(false);
        model.ingest(json!({"sequence":1,"kind":"assistant_message","body":{"text":"hello"}}));
        let rendered = screen(&model, "bad\u{1b}[2J", 40, 10);
        assert_eq!(rendered.lines().count(), 10);
        assert!(!rendered.contains("bad\u{1b}[2J"));
    }

    #[test]
    fn narrow_completed_command_shows_outcome_before_long_identity() {
        let mut model = ViewModel::new(false);
        model.ingest(json!({"sequence":1,"kind":"command_start","phase_id":"a",
            "provider":{"session_id":"one","item_id":"long"},
            "body":{"nested":{"item":{"command":"echo a very long command that exceeds the pane"}}}}));
        model.ingest(json!({"sequence":2,"kind":"command_result","phase_id":"a",
            "provider":{"session_id":"one","item_id":"long"},
            "body":{"nested":{"item":{"exit_code":0}}}}));
        let rendered = screen(&model, "", 30, 8);
        assert!(rendered.contains("Tool completed"));
    }
}
