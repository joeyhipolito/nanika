//! Ratatui layout and widget rendering for the dust dashboard.
//!
//! # Layout
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │ > search query...          [Idle] Ctrl+K: palette | Esc: quit │  ← command bar (3 lines)
//! ├─────────────────────────────────────────────────────────────┤
//! │  Plugin Name         Widget    Description text...           │  ← results list (Fill(1))
//! │  Another Plugin      cmd:foo   More text...                  │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Plugin detail / component tree / action result              │  ← detail pane (Fill(2))
//! └─────────────────────────────────────────────────────────────┘
//! ```

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, AppState};
use crate::component_renderer;

// ── Top-level draw ────────────────────────────────────────────────────────────

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // command bar
            Constraint::Fill(1),   // results list
            Constraint::Fill(2),   // detail pane
        ])
        .split(area);

    render_command_bar(frame, chunks[0], app);
    render_results(frame, chunks[1], app);
    render_detail(frame, chunks[2], app);

    if app.action_palette_open {
        render_action_palette(frame, area, app);
    }
}

// ── Command bar ───────────────────────────────────────────────────────────────

fn render_command_bar(frame: &mut Frame, area: Rect, app: &App) {
    let is_active = matches!(
        app.state,
        AppState::Idle | AppState::Searching | AppState::SelectedCapability
    );
    let border_style = if is_active {
        Style::default().fg(Color::Blue)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let state_label = format!("{:?}", app.state);
    let hint = "  Ctrl+K: palette  Esc: back/quit";
    let title = format!(" Dust  [{state_label}]{hint} ");

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style);

    let query_text = format!("> {}_", app.query);
    frame.render_widget(Paragraph::new(query_text).block(block), area);
}

// ── Results list ──────────────────────────────────────────────────────────────

fn render_results(frame: &mut Frame, area: Rect, app: &App) {
    let title = format!(" Results ({}) ", app.results.len());
    let block = Block::default().borders(Borders::ALL).title(title);

    if app.results.is_empty() {
        let msg = if app.query.trim().is_empty() {
            "No plugins registered — place executables in ~/.dust/plugins/"
        } else {
            "No plugins match your query."
        };
        frame.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(Color::DarkGray))).block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .results
        .iter()
        .map(|(_, manifest)| {
            let caps = App::capability_labels(manifest);
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("  {:<22}", manifest.name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<20}", caps),
                    Style::default().fg(Color::Cyan),
                ),
                Span::styled(
                    format!("  {}", manifest.description),
                    Style::default().fg(Color::DarkGray),
                ),
            ]))
        })
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("► ");

    let mut state = ListState::default();
    state.select(Some(app.selected_index));

    frame.render_stateful_widget(list, area, &mut state);
}

// ── Detail pane ───────────────────────────────────────────────────────────────

fn render_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(" Detail ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match &app.state {
        AppState::Idle if app.results.is_empty() => {
            render_welcome(frame, inner);
        }
        AppState::Idle | AppState::Searching | AppState::SelectedCapability => {
            if let Some((_, manifest)) = app.results.get(app.selected_index) {
                render_manifest_info(frame, inner, manifest);
            } else {
                render_welcome(frame, inner);
            }
        }
        AppState::UILoaded => {
            component_renderer::render(frame, inner, &app.components);
        }
        AppState::ActionDispatched => {
            render_action_result(frame, inner, app);
        }
    }

    // Transient status/error message — overlays the last line of the inner area.
    if let Some(msg) = &app.status_msg {
        if inner.height > 0 {
            let status_rect = Rect {
                x: inner.x,
                y: inner.y + inner.height - 1,
                width: inner.width,
                height: 1,
            };
            frame.render_widget(
                Paragraph::new(Span::styled(msg.clone(), Style::default().fg(Color::Red))),
                status_rect,
            );
        }
    }
}

fn render_welcome(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(Span::styled(
            "Dust Dashboard",
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        )),
        Line::from(""),
        Line::from("  ↑/↓       Navigate plugins"),
        Line::from("  Enter     Render plugin UI"),
        Line::from("  Type      Filter plugins"),
        Line::from("  Esc       Clear query / Quit"),
        Line::from("  Ctrl+K    Open action palette"),
        Line::from(""),
        Line::from(Span::styled(
            "  Place plugin executables in ~/.dust/plugins/ to get started.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        area,
    );
}

fn render_manifest_info(frame: &mut Frame, area: Rect, manifest: &dust_core::PluginManifest) {
    use dust_core::Capability;

    let mut lines = vec![
        Line::from(vec![
            Span::styled(
                manifest.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  v{}", manifest.version),
                Style::default().fg(Color::DarkGray),
            ),
        ]),
        Line::from(""),
        Line::from(manifest.description.clone()),
        Line::from(""),
        Line::from(Span::styled(
            "Capabilities",
            Style::default().add_modifier(Modifier::UNDERLINED),
        )),
    ];

    for cap in &manifest.capabilities {
        let label = match cap {
            Capability::Widget { refresh_secs: 0 } => "  • Widget  (no auto-refresh)".to_string(),
            Capability::Widget { refresh_secs } => {
                format!("  • Widget  (refresh every {}s)", refresh_secs)
            }
            Capability::Command { prefix } => format!("  • Command  prefix: {}", prefix),
            Capability::Scheduler => "  • Scheduler".to_string(),
        };
        lines.push(Line::from(Span::styled(
            label,
            Style::default().fg(Color::Cyan),
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press Enter to render plugin UI",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        area,
    );
}

fn render_action_result(frame: &mut Frame, area: Rect, app: &App) {
    let Some(result) = &app.action_result else {
        return;
    };

    let (status_label, status_style) = if result.success {
        ("✓ Success", Style::default().fg(Color::Green))
    } else {
        ("✗ Failed", Style::default().fg(Color::Red))
    };

    let mut lines = vec![
        Line::from(Span::styled(
            status_label,
            status_style.add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
    ];

    if let Some(msg) = &result.message {
        lines.push(Line::from(format!("  {}", msg)));
        lines.push(Line::from(""));
    }

    if let Some(data) = &result.data {
        lines.push(Line::from(Span::styled(
            "Data",
            Style::default().add_modifier(Modifier::UNDERLINED),
        )));
        let pretty = serde_json::to_string_pretty(data).unwrap_or_else(|_| data.to_string());
        for line in pretty.lines() {
            lines.push(Line::from(format!("  {}", line)));
        }
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "  Press Enter or Esc to continue",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        area,
    );
}

// ── Action palette overlay ────────────────────────────────────────────────────

fn render_action_palette(frame: &mut Frame, area: Rect, app: &App) {
    let popup_area = centered_rect(60, 50, area);
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Action Palette  [Ctrl+K or Esc to close] ")
        .border_style(Style::default().fg(Color::Yellow));

    if let Some((_, manifest)) = app.results.get(app.selected_index) {
        use dust_core::Capability;
        let items: Vec<ListItem> = manifest
            .capabilities
            .iter()
            .enumerate()
            .map(|(i, cap)| {
                let label = match cap {
                    Capability::Widget { .. } => format!("  {}. Render widget", i + 1),
                    Capability::Command { prefix } => {
                        format!("  {}. Execute command  (prefix: {})", i + 1, prefix)
                    }
                    Capability::Scheduler => format!("  {}. Trigger scheduler", i + 1),
                };
                ListItem::new(label)
            })
            .collect();

        if items.is_empty() {
            frame.render_widget(
                Paragraph::new("  No capabilities advertised.")
                    .block(block)
                    .style(Style::default().fg(Color::DarkGray)),
                popup_area,
            );
        } else {
            frame.render_widget(List::new(items).block(block), popup_area);
        }
    } else {
        frame.render_widget(
            Paragraph::new("  No plugin selected.")
                .block(block)
                .style(Style::default().fg(Color::DarkGray)),
            popup_area,
        );
    }
}

// ── Layout helpers ────────────────────────────────────────────────────────────

/// Compute a centered [`Rect`] that is `percent_x` wide and `percent_y` tall
/// relative to `area`.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let margin_y = (100 - percent_y) / 2;
    let margin_x = (100 - percent_x) / 2;

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(margin_y),
            Constraint::Percentage(percent_y),
            Constraint::Percentage(margin_y),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(margin_x),
            Constraint::Percentage(percent_x),
            Constraint::Percentage(margin_x),
        ])
        .split(vertical[1])[1]
}
