//! Maps `dust_core::Component` variants to Ratatui widgets.
//!
//! The single entry point [`render`] accepts a slice of components and renders
//! them sequentially into `area`, building a list of styled [`Line`]s that
//! are displayed in a scrollable [`Paragraph`].

use dust_core::{Component, TextStyle};
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Wrap},
    Frame,
};

// ── Public entry point ────────────────────────────────────────────────────────

/// Render `components` into `area` on `frame`.
///
/// Each `Component` is converted to one or more [`Line`]s:
/// - `Text` → styled single or multi-line paragraph
/// - `List` → bulleted rows, optional bold title above
/// - `Markdown` → plain text with minimal header stripping
/// - `Divider` → a horizontal rule of `─` characters
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

    let mut lines: Vec<Line> = Vec::new();

    for comp in components {
        match comp {
            Component::Text { content, style } => {
                let ratatui_style = map_text_style(style);
                for raw_line in content.lines() {
                    lines.push(Line::from(Span::styled(raw_line.to_string(), ratatui_style)));
                }
            }

            Component::List { items, title } => {
                if let Some(t) = title {
                    lines.push(Line::from(Span::styled(
                        t.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    )));
                }
                for item in items {
                    let bullet = if item.disabled { "  ○ " } else { "  • " };
                    let mut spans = vec![Span::raw(format!("{}{}", bullet, item.label))];
                    if let Some(desc) = &item.description {
                        spans.push(Span::styled(
                            format!("  {}", desc),
                            Style::default().fg(Color::DarkGray),
                        ));
                    }
                    if item.disabled {
                        // Re-render as dimmed
                        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
                        lines.push(Line::from(Span::styled(
                            joined,
                            Style::default().fg(Color::DarkGray),
                        )));
                    } else {
                        lines.push(Line::from(spans));
                    }
                }
            }

            Component::Markdown { content } => {
                for raw_line in content.lines() {
                    lines.push(render_markdown_line(raw_line));
                }
            }

            Component::Divider => {
                let width = area.width.saturating_sub(2) as usize;
                lines.push(Line::from(Span::styled(
                    "─".repeat(width.max(1)),
                    Style::default().fg(Color::DarkGray),
                )));
            }
        }

        // Blank line between components for visual separation.
        lines.push(Line::from(""));
    }

    frame.render_widget(
        Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
        area,
    );
}

// ── Helpers ───────────────────────────────────────────────────────────────────

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
        return Line::from(vec![
            Span::raw("  • "),
            Span::raw(rest.to_string()),
        ]);
    }

    Line::from(line.to_string())
}
