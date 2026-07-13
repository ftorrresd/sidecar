use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};
use unicode_width::UnicodeWidthStr;

use crate::diff::FileDiff;

use super::search::SearchMode;

pub struct Sidebar {
    pub selected_index: usize,
    pub scroll_offset: u16,
}

impl Sidebar {
    pub fn new() -> Self {
        Self {
            selected_index: 0,
            scroll_offset: 0,
        }
    }

    pub fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        files: &[FileDiff],
        search_state: &super::SearchState,
    ) {
        let mut lines: Vec<Line> = Vec::new();

        lines.push(Line::from(Span::styled(
            format!(" Files ({}) ", files.len()),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(""));

        let is_filename_search = search_state.active && search_state.mode == SearchMode::Filename;

        for (i, file) in files.iter().enumerate() {
            let text = if file.path.len() > 50 {
                format!("...{}", &file.path[file.path.len().saturating_sub(47)..])
            } else {
                file.path.clone()
            };

            let add_str = format!("+{}", file.total_additions());
            let del_str = format!("-{}", file.total_deletions());

            let text_width = UnicodeWidthStr::width(text.as_str());
            let padding = area
                .width
                .saturating_sub(text_width as u16)
                .saturating_sub(10);

            let is_selected = if is_filename_search {
                search_state.filename_matches.get(search_state.current_match) == Some(&i)
            } else {
                i == self.selected_index
            };

            let style = if is_selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else if is_filename_search && search_state.filename_matches.contains(&i) {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Gray)
            };

            let spans = vec![
                Span::styled(
                    format!("{} ", add_str),
                    Style::default().fg(if is_selected {
                        Color::Black
                    } else {
                        Color::Green
                    }),
                ),
                Span::styled(
                    format!("{} ", del_str),
                    Style::default().fg(if is_selected { Color::Black } else { Color::Red }),
                ),
                Span::styled(
                    text.clone(),
                    style,
                ),
                Span::styled(
                    " ".repeat(padding as usize),
                    style,
                ),
            ];

            lines.push(Line::from(spans));
        }

        let content_height = area.height.saturating_sub(2) as usize;
        let total_lines = lines.len();
        let max_scroll = total_lines.saturating_sub(content_height) as u16;
        let scroll = self.scroll_offset.min(max_scroll);

        let paragraph = Paragraph::new(lines)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Rounded)
                    .title(" Files ")
                    .style(Style::default()),
            )
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }
}
