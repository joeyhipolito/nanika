//! Maps `dust_core::Component` variants to Ratatui widgets.
//!
//! The single entry point [`render`] accepts a slice of components and renders
//! them sequentially into `area`. Each component is given its own sub-area via
//! `Layout::vertical` so that widget-based components (Table, Gauge) can be
//! rendered alongside line-based ones (Text, List, Markdown, Divider).

use std::collections::HashSet;

use dust_core::{BadgeVariant, Component, DiffLineKind, Hunk, TextStyle, ToolCallStatus};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Gauge, Paragraph, Row, Table, Wrap},
    Frame,
};

/// Op id dispatched on hunk accept. Mirrors `CODE_DIFF_ACCEPT_OP` in
/// `ComponentRenderer.tsx` — keep both sides in sync.
pub const CODE_DIFF_ACCEPT_OP: &str = "code_diff.accept_hunk";

/// `(component_idx, hunk_idx)` of the highlighted CodeDiff hunk, if any.
///
/// The dashboard's input handler owns the cursor and drives `a` / `r`
/// keybindings only when this is `Some` — see `app.rs::handle_key_ui_loaded`.
pub type CodeDiffHunkSelection = Option<(usize, usize)>;

// ── Public entry point ────────────────────────────────────────────────────────

/// Render `components` into `area` on `frame`.
#[allow(dead_code)]
pub fn render(frame: &mut Frame, area: Rect, components: &[Component]) {
    let empty = HashSet::new();
    render_with_full_state(frame, area, components, None, None, &empty);
}

/// Like [`render`], but highlights row `selected_table_row` in the first Table component.
#[allow(dead_code)]
pub fn render_with_selection(
    frame: &mut Frame,
    area: Rect,
    components: &[Component],
    selected_table_row: Option<usize>,
) {
    let empty = HashSet::new();
    render_with_full_state(frame, area, components, selected_table_row, None, &empty);
}

/// Like [`render_with_selection`], but also highlights a hunk inside a
/// [`Component::CodeDiff`]. `selected_code_diff_hunk` is `(component_idx,
/// hunk_idx)` — the same tuple the dashboard's input handler uses to scope
/// `a` / `r` keybindings.
pub fn render_with_selections(
    frame: &mut Frame,
    area: Rect,
    components: &[Component],
    selected_table_row: Option<usize>,
    selected_code_diff_hunk: CodeDiffHunkSelection,
) {
    let empty = HashSet::new();
    render_with_full_state(
        frame,
        area,
        components,
        selected_table_row,
        selected_code_diff_hunk,
        &empty,
    );
}

/// Same as [`render_with_selections`] but additionally accepts a set of
/// `tool_use_id` values that should render in their expanded form.
/// Beats whose id is in the set show params + result; others render
/// collapsed as a single status-dot row.
pub fn render_with_full_state(
    frame: &mut Frame,
    area: Rect,
    components: &[Component],
    selected_table_row: Option<usize>,
    selected_code_diff_hunk: CodeDiffHunkSelection,
    expanded_tool_beats: &HashSet<String>,
) {
    if components.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "No components returned by plugin.",
                Style::default().fg(Color::DarkGray),
            )),
            area,
        );
        return;
    }

    let constraints: Vec<Constraint> = components
        .iter()
        .map(|comp| Constraint::Length(component_height(comp, expanded_tool_beats)))
        .collect();

    let chunks = Layout::vertical(constraints).split(area);

    let mut table_seen = 0usize;
    for (idx, (comp, &chunk)) in components.iter().zip(chunks.iter()).enumerate() {
        if chunk.height == 0 {
            continue;
        }
        let table_sel = if matches!(comp, Component::Table { .. }) {
            let s = if table_seen == 0 { selected_table_row } else { None };
            table_seen += 1;
            s
        } else {
            None
        };
        let hunk_sel = match (comp, selected_code_diff_hunk) {
            (Component::CodeDiff { .. }, Some((cidx, hidx))) if cidx == idx => Some(hidx),
            _ => None,
        };
        render_component(frame, chunk, comp, table_sel, hunk_sel, expanded_tool_beats);
    }
}

// ── Per-component rendering ───────────────────────────────────────────────────

fn render_component(
    frame: &mut Frame,
    area: Rect,
    comp: &Component,
    selected_row: Option<usize>,
    selected_hunk: Option<usize>,
    expanded_tool_beats: &HashSet<String>,
) {
    match comp {
        Component::Text { content, style } => {
            let ratatui_style = map_text_style(style);
            let lines: Vec<Line> = content
                .lines()
                .map(|l| Line::from(Span::styled(l.to_string(), ratatui_style)))
                .collect();
            frame.render_widget(
                Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
                area,
            );
        }

        Component::List { items, title } => {
            let mut lines: Vec<Line> = Vec::new();
            if let Some(t) = title {
                lines.push(Line::from(Span::styled(
                    t.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                )));
            }
            for item in items {
                let bullet = if item.disabled { "  ○ " } else { "  • " };
                let mut spans: Vec<Span> = Vec::new();
                // Render optional icon as a prefix span before the bullet.
                if let Some(icon) = &item.icon {
                    spans.push(Span::raw(format!("{} ", icon)));
                }
                spans.push(Span::raw(format!("{}{}", bullet, item.label)));
                if let Some(desc) = &item.description {
                    spans.push(Span::styled(
                        format!("  {}", desc),
                        Style::default().fg(Color::DarkGray),
                    ));
                }
                if item.disabled {
                    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
                    lines.push(Line::from(Span::styled(
                        joined,
                        Style::default().fg(Color::DarkGray),
                    )));
                } else {
                    lines.push(Line::from(spans));
                }
            }
            frame.render_widget(
                Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
                area,
            );
        }

        Component::Markdown { content } => {
            let lines: Vec<Line> = content.lines().map(render_markdown_line).collect();
            frame.render_widget(
                Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
                area,
            );
        }

        Component::Divider => {
            let width = area.width.saturating_sub(2) as usize;
            frame.render_widget(
                Paragraph::new(Span::styled(
                    "─".repeat(width.max(1)),
                    Style::default().fg(Color::DarkGray),
                )),
                area,
            );
        }

        Component::Table { columns, rows } => {
            let header_cells: Vec<Cell> = columns
                .iter()
                .map(|col| {
                    Cell::from(col.header.clone())
                        .style(Style::default().add_modifier(Modifier::BOLD))
                })
                .collect();
            let header = Row::new(header_cells)
                .style(Style::default().add_modifier(Modifier::UNDERLINED));

            let data_rows: Vec<Row> = rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    let cells = row.iter().map(|cell| Cell::from(cell.clone()));
                    if selected_row == Some(i) {
                        Row::new(cells).style(
                            Style::default()
                                .bg(Color::Blue)
                                .fg(Color::White)
                                .add_modifier(Modifier::BOLD),
                        )
                    } else {
                        Row::new(cells)
                    }
                })
                .collect();

            let col_widths: Vec<Constraint> = columns
                .iter()
                .map(|col| match col.width {
                    Some(w) => Constraint::Length(w as u16),
                    None => Constraint::Fill(1),
                })
                .collect();

            let table = Table::new(data_rows, col_widths)
                .header(header)
                .column_spacing(2);

            // Stateful render so ratatui auto-scrolls the viewport to keep
            // the selected row visible. The row's own highlight styling is
            // already applied above; TableState only drives the offset here.
            if let Some(i) = selected_row {
                let mut state = ratatui::widgets::TableState::default();
                state.select(Some(i));
                frame.render_stateful_widget(table, area, &mut state);
            } else {
                frame.render_widget(table, area);
            }
        }

        Component::KeyValue { pairs } => {
            let max_label = pairs.iter().map(|p| p.label.len()).max().unwrap_or(0);
            let lines: Vec<Line> = pairs
                .iter()
                .map(|pair| {
                    let label_span = Span::styled(
                        format!("{:<width$}", pair.label, width = max_label),
                        Style::default().add_modifier(Modifier::BOLD),
                    );
                    let value_style = match &pair.value_color {
                        Some(c) => Style::default().fg(Color::Rgb(c.r, c.g, c.b)),
                        None => Style::default(),
                    };
                    let value_span = Span::styled(pair.value.clone(), value_style);
                    Line::from(vec![label_span, Span::raw("  "), value_span])
                })
                .collect();
            frame.render_widget(Paragraph::new(Text::from(lines)), area);
        }

        Component::Badge { label, color, variant } => {
            let fg = color
                .as_ref()
                .map(|c| Color::Rgb(c.r, c.g, c.b));

            let (text, style) = match variant {
                BadgeVariant::Filled => {
                    let bg = fg.unwrap_or(Color::White);
                    (
                        format!(" {} ", label),
                        Style::default().bg(bg).fg(Color::Black),
                    )
                }
                BadgeVariant::Outline => {
                    let mut s = Style::default();
                    if let Some(c) = fg {
                        s = s.fg(c);
                    }
                    (format!("[{}]", label), s)
                }
                BadgeVariant::Subtle => {
                    let mut s = Style::default().add_modifier(Modifier::DIM);
                    if let Some(c) = fg {
                        s = s.fg(c);
                    }
                    (label.clone(), s)
                }
                BadgeVariant::Default => {
                    let mut s = Style::default();
                    if let Some(c) = fg {
                        s = s.fg(c);
                    }
                    (label.clone(), s)
                }
            };

            frame.render_widget(Paragraph::new(Span::styled(text, style)), area);
        }

        Component::Progress { value, max, label, color } => {
            let ratio = if *max > 0.0 {
                (value / max).clamp(0.0, 1.0)
            } else {
                0.0
            };

            let gauge_label = label
                .clone()
                .unwrap_or_else(|| format!("{:.0}%", ratio * 100.0));

            let mut gauge_style = Style::default();
            if let Some(c) = color {
                gauge_style = gauge_style.fg(Color::Rgb(c.r, c.g, c.b));
            }

            let gauge = Gauge::default()
                .ratio(ratio)
                .label(gauge_label)
                .gauge_style(gauge_style);

            frame.render_widget(gauge, area);
        }

        Component::AgentTurn { role, content, streaming, .. } => {
            // Intaglio brand colors: terracotta #DA7757 for user, cream #F2EAD7 for assistant.
            let (role_label, role_style) = match role.as_str() {
                "user" => (
                    "you  ",
                    Style::default().fg(Color::Rgb(0xDA, 0x77, 0x57)),
                ),
                _ => (
                    "ai   ",
                    Style::default().fg(Color::Rgb(0xF2, 0xEA, 0xD7)),
                ),
            };

            let content_lines: Vec<&str> = content.lines().collect();
            let mut lines: Vec<Line> = Vec::new();

            if content_lines.is_empty() {
                // Empty content while streaming — show caret on blank line.
                let mut spans = vec![Span::styled(role_label, role_style)];
                if *streaming {
                    spans.push(Span::styled(
                        "▋",
                        Style::default().add_modifier(Modifier::SLOW_BLINK),
                    ));
                }
                lines.push(Line::from(spans));
            } else {
                for (i, line) in content_lines.iter().enumerate() {
                    let prefix = if i == 0 { role_label } else { "     " };
                    let is_last = i == content_lines.len() - 1;
                    let mut spans = vec![
                        Span::styled(prefix, role_style),
                        Span::raw(line.to_string()),
                    ];
                    if *streaming && is_last {
                        spans.push(Span::styled(
                            "▋",
                            Style::default().add_modifier(Modifier::SLOW_BLINK),
                        ));
                    }
                    lines.push(Line::from(spans));
                }
            }

            frame.render_widget(
                Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
                area,
            );
        }

        Component::CodeDiff { path: _, basename, language, hunks } => {
            render_code_diff(frame, area, basename, language.as_deref(), hunks, selected_hunk);
        }

        Component::ToolCallBeat {
            tool_use_id,
            name,
            params,
            result,
            status,
            started_ms,
            finished_ms,
        } => {
            let expanded = expanded_tool_beats.contains(tool_use_id);
            render_tool_call_beat(
                frame,
                area,
                name,
                params.as_ref(),
                result.as_ref(),
                status,
                *started_ms,
                *finished_ms,
                expanded,
            );
        }
        Component::FileRef { path, basename, line } => {
            // Derive basename when omitted: trailing path segment, fallback to
            // the full path for empty/relative inputs.
            let name: String = basename.clone().unwrap_or_else(|| {
                std::path::Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or(path.as_str())
                    .to_string()
            });
            let chip = match line {
                Some(n) => format!("⇒ {name}:{n}"),
                None => format!("⇒ {name}"),
            };
            frame.render_widget(
                Paragraph::new(Span::styled(
                    chip,
                    Style::default()
                        .fg(Color::Rgb(0xDA, 0x77, 0x57))
                        .add_modifier(Modifier::BOLD),
                )),
                area,
            );
        }
    }
}

// ── CodeDiff helpers ──────────────────────────────────────────────────────────

fn render_code_diff(
    frame: &mut Frame,
    area: Rect,
    basename: &str,
    language: Option<&str>,
    hunks: &[Hunk],
    selected_hunk: Option<usize>,
) {
    // Column widths for the two gutter columns (old, new). Derived from the
    // largest line number any hunk will ever render.
    let max_old: u32 = hunks
        .iter()
        .map(|h| h.old_start.saturating_add(h.old_count))
        .max()
        .unwrap_or(1);
    let max_new: u32 = hunks
        .iter()
        .map(|h| h.new_start.saturating_add(h.new_count))
        .max()
        .unwrap_or(1);
    let old_width = digit_width(max_old);
    let new_width = digit_width(max_new);

    // File header line: "basename  ·  lang  ·  N hunks"
    let hunk_word = if hunks.len() == 1 { "hunk" } else { "hunks" };
    let mut header_spans: Vec<Span> = vec![Span::styled(
        basename.to_string(),
        Style::default()
            .fg(Color::Rgb(0xF2, 0xEA, 0xD7))
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(lang) = language {
        header_spans.push(Span::styled(
            format!("  ·  {}", lang),
            Style::default().fg(Color::DarkGray),
        ));
    }
    header_spans.push(Span::styled(
        format!("  ·  {} {}", hunks.len(), hunk_word),
        Style::default().fg(Color::DarkGray),
    ));

    if hunks.is_empty() {
        let empty = vec![
            Line::from(header_spans),
            Line::from(Span::styled(
                "No hunks returned by plugin.".to_string(),
                Style::default().fg(Color::DarkGray),
            )),
        ];
        frame.render_widget(
            Paragraph::new(Text::from(empty)).wrap(Wrap { trim: false }),
            area,
        );
        return;
    }

    if area.height == 0 {
        return;
    }
    // Reserve row 0 for the file header; the remainder is split between hunks.
    let mut constraints: Vec<Constraint> = Vec::with_capacity(1 + hunks.len());
    constraints.push(Constraint::Length(1));
    for h in hunks {
        constraints.push(Constraint::Length(hunk_height(h)));
    }
    let chunks = Layout::vertical(constraints).split(area);

    // Header chunk
    frame.render_widget(
        Paragraph::new(Line::from(header_spans)),
        chunks[0],
    );

    for (i, hunk) in hunks.iter().enumerate() {
        let chunk = chunks.get(i + 1).copied().unwrap_or(Rect::ZERO);
        if chunk.height == 0 {
            continue;
        }
        let is_selected = selected_hunk == Some(i);
        render_hunk(frame, chunk, hunk, old_width, new_width, is_selected);
    }
}

fn render_hunk(
    frame: &mut Frame,
    area: Rect,
    hunk: &Hunk,
    old_width: usize,
    new_width: usize,
    selected: bool,
) {
    let mut lines: Vec<Line<'static>> = Vec::new();

    // Hunk header: @@ -o,oc +n,nc @@  header_name       [a]ccept
    let range = format!(
        "@@ -{},{} +{},{} @@",
        hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
    );
    let mut header_spans: Vec<Span> = vec![
        Span::styled(range, Style::default().fg(Color::DarkGray)),
    ];
    if let Some(h) = &hunk.header {
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(
            h.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    if selected {
        header_spans.push(Span::raw("    "));
        header_spans.push(Span::styled(
            "[a]ccept".to_string(),
            Style::default()
                .fg(Color::Rgb(0xDA, 0x77, 0x57))
                .add_modifier(Modifier::BOLD),
        ));
        header_spans.push(Span::raw("  "));
        header_spans.push(Span::styled(
            "[r]eject".to_string(),
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.push(Line::from(header_spans));

    // Body rows: apply the unified-diff cursor algorithm once.
    let mut old_cursor = hunk.old_start;
    let mut new_cursor = hunk.new_start;
    for line in &hunk.lines {
        let (old_label, new_label, glyph, fg) = match line.kind {
            DiffLineKind::Context => {
                let o = format_line_num(old_cursor, old_width);
                let n = format_line_num(new_cursor, new_width);
                old_cursor += 1;
                new_cursor += 1;
                (o, n, ' ', None)
            }
            DiffLineKind::Add => {
                let n = format_line_num(new_cursor, new_width);
                new_cursor += 1;
                (
                    " ".repeat(old_width),
                    n,
                    '+',
                    Some(Color::Rgb(0xDA, 0x77, 0x57)),
                )
            }
            DiffLineKind::Remove => {
                let o = format_line_num(old_cursor, old_width);
                old_cursor += 1;
                (
                    o,
                    " ".repeat(new_width),
                    '-',
                    Some(Color::Red),
                )
            }
        };
        let mut line_style = Style::default();
        if let Some(c) = fg {
            line_style = line_style.fg(c);
        }
        let gutter_style = Style::default().fg(Color::DarkGray);
        let content_span = Span::styled(line.content.clone(), line_style);
        let row = Line::from(vec![
            Span::styled(format!(" {} ", old_label), gutter_style),
            Span::styled(format!("{} ", new_label), gutter_style),
            Span::styled(format!("{} ", glyph), line_style),
            content_span,
        ]);
        lines.push(row);
    }

    let paragraph = Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false });
    if selected {
        let block = Block::default()
            .borders(Borders::LEFT)
            .border_style(Style::default().fg(Color::Rgb(0xDA, 0x77, 0x57)));
        frame.render_widget(paragraph.block(block), area);
    } else {
        frame.render_widget(paragraph, area);
    }
}

fn hunk_height(hunk: &Hunk) -> u16 {
    // header row + body rows
    let body = hunk.lines.len() as u16;
    body.saturating_add(1)
}

fn digit_width(n: u32) -> usize {
    let mut v = n.max(1);
    let mut w = 0usize;
    while v > 0 {
        v /= 10;
        w += 1;
    }
    w.max(3)
}

fn format_line_num(n: u32, width: usize) -> String {
    format!("{:>width$}", n, width = width)
}

// ── ToolCallBeat helpers ──────────────────────────────────────────────────────

fn status_color(status: &ToolCallStatus) -> Color {
    match status {
        ToolCallStatus::Pending | ToolCallStatus::Running => Color::Rgb(0xDA, 0x77, 0x57),
        ToolCallStatus::Ok => Color::DarkGray,
        ToolCallStatus::Err => Color::Red,
    }
}

fn status_label(status: &ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Pending => "pending",
        ToolCallStatus::Running => "running",
        ToolCallStatus::Ok => "ok",
        ToolCallStatus::Err => "err",
    }
}

fn format_elapsed_ms(started_ms: u64, finished_ms: Option<u64>) -> String {
    let end = finished_ms.unwrap_or_else(|| {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(started_ms)
    });
    let ms = end.saturating_sub(started_ms);
    if ms < 1_000 {
        format!("{}ms", ms)
    } else {
        format!("{:.1}s", ms as f64 / 1_000.0)
    }
}

fn tool_call_beat_height(
    params: Option<&serde_json::Value>,
    result: Option<&serde_json::Value>,
    expanded: bool,
) -> u16 {
    if !expanded {
        return 1;
    }
    // Header row + divider + params rows + result rows
    let mut rows: u32 = 1;
    if let Some(p) = params {
        rows += 1; // "params:" label
        rows += json_display_lines(p).len() as u32;
    }
    if let Some(r) = result {
        rows += 1; // "result:" label
        rows += json_display_lines(r).len() as u32;
    }
    rows.min(u16::MAX as u32) as u16
}

fn json_display_lines(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::Object(map) if !map.is_empty() => map
            .iter()
            .map(|(k, val)| format!("  {}: {}", k, scalar_to_string(val)))
            .collect(),
        _ => serde_json::to_string_pretty(v)
            .unwrap_or_default()
            .lines()
            .map(|l| format!("  {}", l))
            .collect(),
    }
}

fn scalar_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".into(),
        _ => v.to_string(),
    }
}

fn render_tool_call_beat(
    frame: &mut Frame,
    area: Rect,
    name: &str,
    params: Option<&serde_json::Value>,
    result: Option<&serde_json::Value>,
    status: &ToolCallStatus,
    started_ms: u64,
    finished_ms: Option<u64>,
    expanded: bool,
) {
    let dot_color = status_color(status);
    let elapsed = format_elapsed_ms(started_ms, finished_ms);
    let mut dot_style = Style::default().fg(dot_color);
    if matches!(status, ToolCallStatus::Running | ToolCallStatus::Pending) {
        dot_style = dot_style.add_modifier(Modifier::SLOW_BLINK);
    }
    let indicator = if expanded { "▾" } else { "▸" };
    let header = Line::from(vec![
        Span::styled(format!("{} ", indicator), Style::default().fg(Color::DarkGray)),
        Span::styled("● ", dot_style),
        Span::styled(
            name.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("({}, {})", status_label(status), elapsed),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    if !expanded {
        frame.render_widget(Paragraph::new(header), area);
        return;
    }

    let mut lines: Vec<Line<'static>> = vec![header];

    if let Some(p) = params {
        lines.push(Line::from(Span::styled(
            "params:".to_string(),
            Style::default().fg(Color::DarkGray),
        )));
        for l in json_display_lines(p) {
            lines.push(Line::from(Span::raw(l)));
        }
    }

    if let Some(r) = result {
        let label_color = if matches!(status, ToolCallStatus::Err) {
            Color::Red
        } else {
            Color::DarkGray
        };
        lines.push(Line::from(Span::styled(
            "result:".to_string(),
            Style::default().fg(label_color),
        )));
        for l in json_display_lines(r) {
            let style = if matches!(status, ToolCallStatus::Err) {
                Style::default().fg(Color::Red)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(l, style)));
        }
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );
}

// ── Height estimation ─────────────────────────────────────────────────────────

/// Estimate the terminal row height required to render `comp`.
///
/// Includes a one-row blank separator after each component. Heights are
/// intentionally generous — the layout clips any overflow at the area boundary.
fn component_height(comp: &Component, expanded_tool_beats: &HashSet<String>) -> u16 {
    let content_height: u16 = match comp {
        Component::Text { content, .. } => content.lines().count() as u16,
        Component::List { items, title } => {
            items.len() as u16 + if title.is_some() { 1 } else { 0 }
        }
        Component::Markdown { content } => content.lines().count() as u16,
        Component::Divider => 1,
        Component::Table { rows, .. } => {
            // header row + data rows; Table widget draws its own borders
            1 + rows.len() as u16
        }
        Component::KeyValue { pairs } => pairs.len() as u16,
        Component::Badge { .. } => 1,
        Component::Progress { .. } => 1,
        Component::AgentTurn { content, .. } => content.lines().count().max(1) as u16,
        Component::CodeDiff { hunks, .. } => {
            // 1 file header + per hunk: 1 header + N lines
            let mut rows: u32 = 1;
            for h in hunks {
                rows += 1 + h.lines.len() as u32;
            }
            rows.min(u16::MAX as u32) as u16
        }
        Component::ToolCallBeat {
            tool_use_id,
            params,
            result,
            ..
        } => {
            let expanded = expanded_tool_beats.contains(tool_use_id);
            tool_call_beat_height(params.as_ref(), result.as_ref(), expanded)
        }
        // FileRef is always one row in the ratatui surface; the ±10-line
        // preview happens inline in chat scrollback rather than claiming
        // reserved component height (expansion is host-side driven).
        Component::FileRef { .. } => 1,
    };
    // +1 blank separator between components
    content_height.saturating_add(1).max(1)
}

// ── Style helpers ─────────────────────────────────────────────────────────────

fn map_text_style(style: &TextStyle) -> Style {
    let mut s = Style::default();
    if style.bold {
        s = s.add_modifier(Modifier::BOLD);
    }
    if style.italic {
        s = s.add_modifier(Modifier::ITALIC);
    }
    if style.underline {
        s = s.add_modifier(Modifier::UNDERLINED);
    }
    if let Some(color) = &style.color {
        s = s.fg(Color::Rgb(color.r, color.g, color.b));
    }
    s
}

/// Very basic markdown line rendering: strip leading `#` markers for headers,
/// render `---` as a visual divider hint, pass everything else through as-is.
fn render_markdown_line(line: &str) -> Line<'static> {
    let trimmed = line.trim_start();

    // ATX-style headings: # Heading, ## Heading, etc.
    if let Some(rest) = trimmed.strip_prefix("### ") {
        return Line::from(Span::styled(
            rest.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("## ") {
        return Line::from(Span::styled(
            rest.to_string(),
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::UNDERLINED),
        ));
    }
    if let Some(rest) = trimmed.strip_prefix("# ") {
        return Line::from(Span::styled(
            rest.to_string(),
            Style::default()
                .add_modifier(Modifier::BOLD)
                .fg(Color::Cyan),
        ));
    }

    // Thematic break / horizontal rule
    if trimmed == "---" || trimmed == "***" || trimmed == "___" {
        return Line::from(Span::styled(
            "─".repeat(40),
            Style::default().fg(Color::DarkGray),
        ));
    }

    // Bullet lists
    if let Some(rest) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
        return Line::from(vec![Span::raw("  • "), Span::raw(rest.to_string())]);
    }

    Line::from(line.to_string())
}
