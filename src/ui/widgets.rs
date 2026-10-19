//! Reusable widgets: checklists and text fields.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

/// One rendered line of a checkbox list.
pub struct CheckRow {
    pub text: String,
    pub selected: bool,
}

/// Renders a scrollable checkbox list: `[x] name - detail`, with the row under
/// the cursor highlighted in yellow.
pub fn draw_checklist(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    rows: &[CheckRow],
    cursor: usize,
) {
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let visible = inner.height as usize;
    // Scroll window that always keeps the cursor row on screen.
    let first = cursor.saturating_sub(visible.saturating_sub(1));

    for (index, row) in rows.iter().enumerate().skip(first).take(visible) {
        let marker = if row.selected { "[x]" } else { "[ ]" };
        let text = format!("{} {}", marker, row.text);
        let style = if index == cursor {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else if row.selected {
            Style::default().fg(Color::Green)
        } else {
            Style::default()
        };
        let line_area = Rect {
            x: inner.x,
            y: inner.y + (index - first) as u16,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(text).style(style), line_area);
    }
}

/// One single-line text box of the Custom step. Long text scrolls so the end
/// (where typing happens) always stays visible.
pub fn draw_field(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: &str,
    focused: bool,
    typing: bool,
    invalid: bool,
) {
    let border_color = if invalid {
        Color::Red
    } else if focused && typing {
        Color::Green
    } else if focused {
        Color::Yellow
    } else {
        Color::Reset
    };

    let inner_width = area.width.saturating_sub(2) as usize;
    let chars: Vec<char> = text.chars().collect();
    let first_visible = chars.len().saturating_sub(inner_width.saturating_sub(1));
    let shown: String = chars[first_visible..].iter().collect();

    let paragraph = Paragraph::new(shown)
        .style(Style::default().fg(if focused && typing {
            Color::Green
        } else {
            Color::Reset
        }))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        );
    frame.render_widget(paragraph, area);

    if focused && typing && area.width > 2 && area.height > 2 {
        let column = (chars.len() - first_visible).min(inner_width.saturating_sub(1)) as u16;
        frame.set_cursor(area.x + 1 + column, area.y + 1);
    }
}

pub fn draw_input(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    text: &str,
    is_typing: bool,
    error: Option<&str>,
    note: Option<&str>,
) {
    let border_color = if error.is_some() {
        Color::Red
    } else if is_typing {
        Color::Green
    } else {
        Color::Reset
    };

    let inner_width = area.width.saturating_sub(2).max(1) as usize;
    let inner_height = area.height.saturating_sub(2).max(1) as usize;

    // Hard-wrap ourselves so the caret math below matches exactly what is
    // rendered (ratatui's word wrapper breaks long paths at different spots).
    let mut lines: Vec<Line> = hard_wrap(text, inner_width)
        .into_iter()
        .map(|line| Line::from(line))
        .collect();
    if let Some(err) = error {
        lines.push(Line::from(""));
        lines.push(Line::from(format!("[WARN] {}", err)).style(Style::default().fg(Color::Red)));
    }
    if let Some(note) = note {
        lines.push(Line::from(""));
        lines
            .push(Line::from(format!("[INFO] {}", note)).style(Style::default().fg(Color::Yellow)));
    }

    // Where the caret sits once the text wraps, plus the scroll needed to keep
    // that row inside the visible area.
    let cursor_chars = text.chars().count();
    let cursor_row = cursor_chars / inner_width;
    let cursor_col = cursor_chars % inner_width;
    let scroll_y = cursor_row.saturating_sub(inner_height.saturating_sub(1)) as u16;

    let paragraph = Paragraph::new(lines)
        .style(Style::default().fg(if is_typing {
            Color::Green
        } else {
            Color::Reset
        }))
        .wrap(Wrap { trim: false })
        .scroll((scroll_y, 0))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        );
    frame.render_widget(paragraph, area);

    if is_typing && area.width > 2 && area.height > 2 {
        let row_on_screen = cursor_row - scroll_y as usize;
        frame.set_cursor(
            area.x + 1 + cursor_col as u16,
            area.y + 1 + row_on_screen as u16,
        );
    }
}

/// Hard-wraps `text` at `width` characters, returning at least one line so the
/// caret always has a row to sit on.
fn hard_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut column = 0usize;

    // Iterate Unicode scalar values directly rather than first allocating a
    // second Vec<char> the size of the entire input.
    for ch in text.chars() {
        line.push(ch);
        column += 1;
        if column == width {
            lines.push(std::mem::take(&mut line));
            column = 0;
        }
    }

    // Keep the trailing empty row when text exactly fills the final row; the
    // editor uses it as the row on which the caret can sit.
    if !line.is_empty() || lines.is_empty() || column == 0 {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_wrap_handles_unicode_and_trailing_caret_row() {
        assert_eq!(hard_wrap("abcdef", 3), vec!["abc", "def", ""]);
        assert_eq!(hard_wrap("abcde", 3), vec!["abc", "de"]);
        assert_eq!(hard_wrap("a🦀bc", 2), vec!["a🦀", "bc", ""]);
        assert_eq!(hard_wrap("", 3), vec![""]);
        assert_eq!(hard_wrap("abc", 0), vec!["a", "b", "c", ""]);
    }
}
