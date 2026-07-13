use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::search::{SearchMode, SearchState};

pub struct StatusBar;

impl StatusBar {
    pub fn render(
        f: &mut Frame,
        area: Rect,
        search: &SearchState,
        layout: &str,
        focused: &str,
        show_sidebar: bool,
        theme: &str,
    ) {
        if search.active {
            let prompt = match search.mode {
                SearchMode::Filename => "File search: ",
                SearchMode::Content => "Search: ",
            };

            let mut spans = vec![Span::styled(
                prompt,
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            )];

            spans.push(Span::styled(&search.query, Style::default().fg(Color::White)));

            let indicator = if search.query.is_empty() {
                " ".to_string()
            } else {
                format!(
                    " [{}/{}]",
                    search.current_match + 1,
                    search.filename_matches.len().max(1)
                )
            };
            spans.push(Span::styled(indicator, Style::default().fg(Color::Yellow)));

            let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::DarkGray));
            f.render_widget(paragraph, area);
        } else {
            let sidebar_label = if show_sidebar { "s:hide-list" } else { "s:show-list" };
            let help = format!(
                " q:quit | jk/↓↑:nav | hl/←→:scroll | s:{} | t:{} | /:search | F:file | L:lines | v:{}({})",
                sidebar_label, theme, layout, focused
            );

            let paragraph = Paragraph::new(Line::from(Span::styled(
                help,
                Style::default().fg(Color::White).bg(Color::DarkGray),
            )));
            f.render_widget(paragraph, area);
        }
    }
}
