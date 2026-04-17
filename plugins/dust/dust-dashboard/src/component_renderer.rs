//! Maps `dust_core::Component` variants to Ratatui widgets.
//!
//! The single entry point [`render`] accepts a slice of components and renders
//! them sequentially into `area`. Each component is given its own sub-area via
//! `Layout::vertical` so that widget-based components (Table, Gauge) can be
//! rendered alongside line-based ones (Text, List, Markdown, Divider).

use dust_core::{BadgeVariant, Component, TextStyle};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Cell, Gauge, Paragraph, Row, Table, Wrap},
    Frame,
};

// ── Public entry point ────────────────────────────────────────────────────────

/// Render `components` into `area` on `frame`.
///
/// Each `Component` is assigned a height via [`component_height`], the area is
/// split with `Layout::vertical`, and each component is rendered into its own
/// sub-rect by [`render_component`].
pub fn render(frame: &mut Frame, area: Rect, components: &[Component]) {
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
        .map(|comp| Constraint::Length(component_height(comp)))
        .collect();

    let chunks = Layout::vertical(constraints).split(area);

    for (comp, &chunk) in components.iter().zip(chunks.iter()) {
        if chunk.height == 0 {
            continue;
        }
        render_component(frame, chunk, comp);
    }
}

// ── Per-component rendering ───────────────────────────────────────────────────

fn render_component(frame: &mut Frame, area: Rect, comp: &Component) {
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
                .map(|row| Row::new(row.iter().map(|cell| Cell::from(cell.clone()))))
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

            frame.render_widget(table, area);
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
    }
}

// ── Height estimation ─────────────────────────────────────────────────────────

/// Estimate the terminal row height required to render `comp`.
///
/// Includes a one-row blank separator after each component. Heights are
/// intentionally generous — the layout clips any overflow at the area boundary.
fn component_height(comp: &Component) -> u16 {
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
