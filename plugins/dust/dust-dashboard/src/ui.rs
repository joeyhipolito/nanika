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

    // Chat view takes over the full terminal area.
    if app.state == AppState::Chatting {
        render_chat_screen(frame, area, app);
        return;
    }

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

    match app.state {
        AppState::PaletteOpen => render_action_palette(frame, area, app),
        AppState::PaletteCollectingArg => render_arg_collection(frame, area, app),
        _ => {}
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
    let hint = if app.detail_open {
        "  ↑/↓: row · Esc: close pane · Ctrl+K: palette"
    } else if matches!(app.state, AppState::UILoaded | AppState::ActionDispatched) {
        "  ↑/↓: row · Tab/→: detail · Ctrl+K: palette · Esc: back"
    } else {
        "  Ctrl+K: palette  Esc: back/quit"
    };
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
        AppState::Chatting => {
            // draw() returns early for Chatting; this arm is unreachable.
        }
        AppState::UILoaded
        | AppState::PaletteOpen
        | AppState::PaletteCollectingArg => {
            if app.detail_open {
                let h_chunks = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                    .split(inner);
                component_renderer::render_with_selections(
                    frame,
                    h_chunks[0],
                    &app.components,
                    Some(app.table_cursor),
                    app.code_diff_cursor,
                );
                render_issue_detail(frame, h_chunks[1], app);
            } else {
                component_renderer::render_with_selections(
                    frame,
                    inner,
                    &app.components,
                    Some(app.table_cursor),
                    app.code_diff_cursor,
                );
            }
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

// ── Issue detail pane ─────────────────────────────────────────────────────────

fn render_issue_detail(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Issue Detail  Esc: close ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(issue) = app.selected_issue_detail() else {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "No issue selected.",
                Style::default().fg(Color::DarkGray),
            )),
            inner,
        );
        return;
    };

    let mut lines: Vec<Line> = Vec::new();

    // ID + Title
    let id = issue
        .get("seq_id")
        .and_then(|v| v.as_i64())
        .map(|n| format!("TRK-{n}"))
        .unwrap_or_else(|| "—".to_string());
    let title = issue.get("title").and_then(|v| v.as_str()).unwrap_or("—");
    lines.push(Line::from(vec![
        Span::styled(
            format!("[{id}] "),
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(title.to_string(), Style::default().add_modifier(Modifier::BOLD)),
    ]));
    lines.push(Line::from(""));

    // Status + Priority
    let status = issue.get("status").and_then(|v| v.as_str()).unwrap_or("—");
    let priority = issue.get("priority").and_then(|v| v.as_str()).unwrap_or("—");
    lines.push(Line::from(vec![
        Span::styled("Status   ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(status.to_string()),
        Span::raw("   "),
        Span::styled("Pri ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(priority.to_string()),
    ]));

    // Labels
    let labels = issue
        .get("labels")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if !labels.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Labels   ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(labels.to_string(), Style::default().fg(Color::Cyan)),
        ]));
    }

    // Parent
    let issue_id = issue.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if let Some(pid) = issue.get("parent_id").and_then(|v| v.as_str()) {
        let parent = app
            .cached_issues
            .iter()
            .find(|i| i.get("id").and_then(|v| v.as_str()) == Some(pid));
        let parent_seq = parent
            .and_then(|i| i.get("seq_id").and_then(|v| v.as_i64()))
            .map(|n| format!("TRK-{n}"))
            .unwrap_or_else(|| pid[..8.min(pid.len())].to_string());
        let parent_title = parent
            .and_then(|i| i.get("title").and_then(|v| v.as_str()))
            .unwrap_or("—");
        lines.push(Line::from(vec![
            Span::styled("Parent   ", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("[{parent_seq}] {parent_title}"),
                Style::default().fg(Color::Green),
            ),
        ]));
    }

    // Children
    let children: Vec<String> = app
        .cached_issues
        .iter()
        .filter_map(|i| {
            if i.get("parent_id").and_then(|v| v.as_str()) == Some(issue_id) {
                let cid = i
                    .get("seq_id")
                    .and_then(|v| v.as_i64())
                    .map(|n| format!("TRK-{n}"))
                    .unwrap_or_else(|| "?".to_string());
                let ctitle = i.get("title").and_then(|v| v.as_str()).unwrap_or("?");
                Some(format!("[{cid}] {ctitle}"))
            } else {
                None
            }
        })
        .collect();
    if !children.is_empty() {
        lines.push(Line::from(Span::styled(
            "Children",
            Style::default().add_modifier(Modifier::BOLD),
        )));
        for child in &children {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(child.clone(), Style::default().fg(Color::Blue)),
            ]));
        }
    }

    // Blockers (from cached_issues links — not available from tree, show placeholder)
    lines.push(Line::from(""));

    // Description
    lines.push(Line::from(Span::styled(
        "Description",
        Style::default().add_modifier(Modifier::UNDERLINED),
    )));
    let desc = issue
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if desc.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (none)",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for line in desc.lines() {
            lines.push(Line::from(format!("  {}", line)));
        }
    }

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
        inner,
    );
}

// ── Action palette overlay ────────────────────────────────────────────────────

fn render_action_palette(frame: &mut Frame, area: Rect, app: &App) {
    let popup_area = centered_rect(60, 60, area);
    frame.render_widget(Clear, popup_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Action Palette  ↑/↓ select · Enter confirm · Esc cancel ")
        .border_style(Style::default().fg(Color::Yellow));

    let ops = App::tracker_ops();
    let items: Vec<ListItem> = ops
        .iter()
        .enumerate()
        .map(|(i, (op_id, label, args))| {
            let arg_hint = if args.is_empty() {
                String::new()
            } else {
                format!("  ({})", args.join(", "))
            };
            let style = if i == app.palette_ctx.cursor {
                Style::default()
                    .bg(Color::Yellow)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            ListItem::new(Line::from(vec![
                Span::styled(format!("  {:8} ", op_id), style),
                Span::styled(label.to_string(), style),
                Span::styled(arg_hint, style.fg(Color::DarkGray)),
            ]))
        })
        .collect();

    let mut list_state = ListState::default();
    list_state.select(Some(app.palette_ctx.cursor));
    frame.render_stateful_widget(List::new(items).block(block), popup_area, &mut list_state);
}

// ── Arg collection overlay ────────────────────────────────────────────────────

fn render_arg_collection(frame: &mut Frame, area: Rect, app: &App) {
    let popup_area = centered_rect(60, 40, area);
    frame.render_widget(Clear, popup_area);

    let ops = App::tracker_ops();
    let (op_id, label, arg_prompts) = ops[app.palette_ctx.op_idx];
    let arg_idx = app.palette_ctx.arg_idx;
    let prompt = arg_prompts.get(arg_idx).copied().unwrap_or("Value");

    let title = format!(
        " {op_id} — {label}  [arg {}/{total}] · Enter confirm · Esc cancel ",
        arg_idx + 1,
        total = arg_prompts.len()
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Cyan));

    let mut lines: Vec<Line> = Vec::new();

    // Already-collected args.
    for (i, val) in app.palette_ctx.collected.iter().enumerate() {
        let done_prompt = arg_prompts.get(i).copied().unwrap_or("?");
        lines.push(Line::from(vec![
            Span::styled(
                format!("  {:<20} ", done_prompt),
                Style::default().fg(Color::DarkGray),
            ),
            Span::styled(val.clone(), Style::default().fg(Color::Green)),
        ]));
    }
    lines.push(Line::from(""));

    // Current arg prompt + buffer.
    lines.push(Line::from(vec![
        Span::styled(
            format!("  {:<20} ", prompt),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{}_", app.palette_ctx.buffer),
            Style::default().fg(Color::Yellow),
        ),
    ]));

    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup_area,
    );
}

// ── Chat screen ───────────────────────────────────────────────────────────────

/// Full-terminal chat view entered via `/`.
///
/// Layout:
/// ```text
/// ┌─ Chat header (3 rows) ──────────────────────────────────────────────────┐
/// │ [Threads column 30%] │ Message/AgentTurn area (70%)                     │
/// ├─────────────────────────────────────────────────────────────────────────┤
/// │ Input bar (3 rows)                                                       │
/// └─────────────────────────────────────────────────────────────────────────┘
/// ```
fn render_chat_screen(frame: &mut Frame, area: Rect, app: &App) {
    let v_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // header
            Constraint::Fill(1),   // threads + messages
            Constraint::Length(3), // input bar
        ])
        .split(area);

    // ── Header ────────────────────────────────────────────────────────────────
    let thread_label = app
        .chat_active_thread_id
        .as_deref()
        .map(|id| format!("thread:{}", &id[..8.min(id.len())]))
        .unwrap_or_else(|| "new thread".to_string());
    let title = format!(
        " Chat  [{}]  Esc: quit · Enter: send · Ctrl+N: new · Ctrl+T: threads ",
        thread_label
    );
    let header_block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Magenta));
    frame.render_widget(Paragraph::new("").block(header_block), v_chunks[0]);

    // ── Main area ─────────────────────────────────────────────────────────────
    if app.chat_show_threads && !app.chat_threads.is_empty() {
        let h_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
            .split(v_chunks[1]);
        render_chat_threads(frame, h_chunks[0], app);
        component_renderer::render_with_full_state(
            frame,
            h_chunks[1],
            &app.chat_messages,
            None,
            None,
            &app.tool_call_expanded,
        );
    } else {
        component_renderer::render_with_full_state(
            frame,
            v_chunks[1],
            &app.chat_messages,
            None,
            None,
            &app.tool_call_expanded,
        );
    }

    // ── Input bar ─────────────────────────────────────────────────────────────
    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(" Message ")
        .border_style(Style::default().fg(Color::Blue));
    let input_text = format!("> {}_", app.chat_input);
    frame.render_widget(Paragraph::new(input_text).block(input_block), v_chunks[2]);

    // Transient status/error message — overlays the last line of the main area.
    if let Some(msg) = &app.status_msg {
        let inner = v_chunks[1];
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

fn render_chat_threads(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Threads (Ctrl+T) ")
        .border_style(Style::default().fg(Color::Cyan));

    if app.chat_threads.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "No threads yet.",
                Style::default().fg(Color::DarkGray),
            ))
            .block(block),
            area,
        );
        return;
    }

    let items: Vec<ListItem> = app
        .chat_threads
        .iter()
        .map(|t| {
            let title = t.get("title").and_then(|v| v.as_str()).unwrap_or("Untitled");
            ListItem::new(Line::from(format!("  {title}")))
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
    state.select(Some(app.chat_thread_cursor));
    frame.render_stateful_widget(list, area, &mut state);
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
