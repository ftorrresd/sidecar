use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use crate::diff::{DiffLineType, FileDiff};

use super::Highlighter;

pub struct DiffView {
    pub scroll_offset: u16,
    pub h_scroll: u16,
}

impl DiffView {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0,
            h_scroll: 0,
        }
    }

    fn build_spans(
        tokens: &[(String, Color)],
        search_state: &super::SearchState,
    ) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        let searching = search_state.active
            && search_state.mode == super::search::SearchMode::Content
            && !search_state.query.is_empty();

        for (text, color) in tokens {
            let base_style = Style::default().fg(*color);
            if searching {
                let lower_text = text.to_lowercase();
                let lower_query = search_state.query.to_lowercase();
                let mut pos = 0;
                loop {
                    let start = lower_text[pos..].find(&lower_query);
                    match start {
                        None => {
                            if pos < text.len() {
                                spans.push(Span::styled(
                                    text[pos..].to_string(),
                                    base_style,
                                ));
                            }
                            break;
                        }
                        Some(offset) => {
                            let abs_start = pos + offset;
                            let abs_end = abs_start + search_state.query.len();
                            if abs_start > pos {
                                spans.push(Span::styled(
                                    text[pos..abs_start].to_string(),
                                    base_style,
                                ));
                            }
                            spans.push(Span::styled(
                                text[abs_start..abs_end].to_string(),
                                Style::default().bg(Color::Yellow).fg(Color::Black),
                            ));
                            pos = abs_end;
                        }
                    }
                }
            } else {
                spans.push(Span::styled(text.clone(), base_style));
            }
        }
        spans
    }

    pub fn render(
        &self,
        f: &mut Frame,
        area: Rect,
        file: &FileDiff,
        search_state: &super::SearchState,
        show_line_numbers: bool,
        highlighter: &mut Highlighter,
    ) {
        let mut lines: Vec<Line> = Vec::new();

        let file_header = format!(
            " {} [+{}-{}] ",
            file.path,
            file.total_additions(),
            file.total_deletions()
        );
        lines.push(Line::from(Span::styled(
            file_header,
            Style::default().fg(Color::White).bg(Color::DarkGray),
        )));
        lines.push(Line::from(""));

        for hunk in &file.hunks {
            lines.push(Line::from(Span::styled(
                &hunk.header,
                Style::default().fg(Color::Cyan),
            )));

            for diff_line in &hunk.lines {
                let (prefix_color, prefix) = match diff_line.kind {
                    DiffLineType::Add => (Color::Green, "+"),
                    DiffLineType::Remove => (Color::Red, "-"),
                    DiffLineType::Context => (Color::DarkGray, " "),
                };

                let content = diff_line.content.replace('\t', "    ");
                let tokens = highlighter.highlight_line(&content, &file.path);

                let mut spans = Vec::new();

                if show_line_numbers {
                    let old_str = diff_line
                        .old_line
                        .map(|n| format!("{:>4}", n))
                        .unwrap_or_else(|| "    ".to_string());
                    let new_str = diff_line
                        .new_line
                        .map(|n| format!("{:<4}", n))
                        .unwrap_or_else(|| "    ".to_string());

                    spans.push(Span::styled(
                        format!("{} {} ", old_str, new_str),
                        Style::default().fg(Color::DarkGray),
                    ));
                }

                spans.push(Span::styled(prefix, Style::default().fg(prefix_color)));
                spans.extend(Self::build_spans(&tokens, search_state));

                lines.push(Line::from(spans));
            }
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
                    .style(Style::default()),
            )
            .scroll((scroll, self.h_scroll))
            .wrap(Wrap { trim: false });

        f.render_widget(paragraph, area);
    }
}
