use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind};
use notify::{EventKind, RecursiveMode, Watcher};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame, Terminal,
};
use std::path::PathBuf;
use std::sync::mpsc;

use crate::config::Config;
use crate::diff::FileDiff;
use crate::ui::{self, search::SearchMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Auto,
    Split,
    Stack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Diff,
}

pub struct App {
    pub files: Vec<FileDiff>,
    pub sidebar: ui::Sidebar,
    pub diff_view: ui::DiffView,
    pub search_state: ui::SearchState,
    pub highlighter: ui::Highlighter,
    pub layout_mode: LayoutMode,
    pub focus: Focus,
    pub line_numbers: bool,
    pub menu_bar: bool,
    pub show_sidebar: bool,
    pub quit: bool,
    #[allow(dead_code)]
    pub config: Config,
    pub watch_paths: Option<Vec<PathBuf>>,
    pub reload_tx: Option<mpsc::Sender<()>>,
}

impl App {
    pub fn new(files: Vec<FileDiff>, watch_paths: Option<Vec<PathBuf>>) -> Self {
        let config = Config::load();
        let line_numbers = config.line_numbers.unwrap_or(true);
        let menu_bar = config.menu_bar.unwrap_or(true);
        let layout_mode = match config.mode.as_deref() {
            Some("stack") => LayoutMode::Stack,
            Some("auto") => LayoutMode::Auto,
            _ => LayoutMode::Split,
        };

        Self {
            files,
            sidebar: ui::Sidebar::new(),
            diff_view: ui::DiffView::new(),
            search_state: ui::SearchState::new(),
            highlighter: ui::Highlighter::new(),
            layout_mode,
            focus: if layout_mode == LayoutMode::Stack {
                Focus::Diff
            } else {
                Focus::Sidebar
            },
            line_numbers,
            menu_bar,
            show_sidebar: true,
            quit: false,
            config,
            watch_paths,
            reload_tx: None,
        }
    }

    pub fn current_file(&self) -> Option<&FileDiff> {
        if self.search_state.active && self.search_state.mode == SearchMode::Filename {
            self.search_state
                .filename_matches
                .get(self.search_state.current_match)
                .and_then(|&i| self.files.get(i))
        } else {
            self.files.get(self.sidebar.selected_index)
        }
    }

    fn move_selection(&mut self, delta: i32) {
        if self.search_state.active && self.search_state.mode == SearchMode::Filename {
            if !self.search_state.filename_matches.is_empty() {
                let current = self.search_state.current_match as i32;
                let len = self.search_state.filename_matches.len() as i32;
                self.search_state.current_match = ((current + delta).rem_euclid(len)) as usize;
            }
        } else {
            let new = (self.sidebar.selected_index as i32 + delta).max(0) as usize;
            self.sidebar.selected_index = new.min(self.files.len().saturating_sub(1));
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind == KeyEventKind::Release {
            return;
        }

        if self.search_state.active {
            match key.code {
                KeyCode::Esc => {
                    self.search_state.clear();
                }
                KeyCode::Enter => {
                    self.search_state.active = false;
                    if self.search_state.mode == SearchMode::Filename
                        && !self.search_state.filename_matches.is_empty()
                    {
                        let idx =
                            self.search_state.filename_matches[self.search_state.current_match];
                        self.sidebar.selected_index = idx;
                    }
                }
                KeyCode::Backspace => {
                    self.search_state.pop_char();
                    if self.search_state.mode == SearchMode::Filename {
                        self.refresh_name_matches();
                    }
                }
                KeyCode::Char(c) => {
                    self.search_state.push_char(c);
                    if self.search_state.mode == SearchMode::Filename {
                        self.refresh_name_matches();
                    }
                }
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Char('q') => {
                self.quit = true;
            }
            KeyCode::Char('j') | KeyCode::Down => match self.focus {
                Focus::Sidebar => self.move_selection(1),
                Focus::Diff => {
                    self.diff_view.scroll_offset =
                        self.diff_view.scroll_offset.saturating_add(1);
                }
            },
            KeyCode::Char('k') | KeyCode::Up => match self.focus {
                Focus::Sidebar => self.move_selection(-1),
                Focus::Diff => {
                    self.diff_view.scroll_offset =
                        self.diff_view.scroll_offset.saturating_sub(1);
                }
            },
            KeyCode::Char('g') => {
                self.diff_view.scroll_offset = 0;
                self.diff_view.h_scroll = 0;
                self.sidebar.selected_index = 0;
                self.sidebar.scroll_offset = 0;
            }
            KeyCode::Char('G') => {
                if let Some(file) = self.current_file() {
                    let total_lines: usize =
                        file.hunks.iter().map(|h| h.lines.len() + 1).sum();
                    let content_height = 20;
                    self.diff_view.scroll_offset =
                        total_lines.saturating_sub(content_height) as u16;
                }
            }
            KeyCode::Char('s') => {
                self.show_sidebar = !self.show_sidebar;
                if !self.show_sidebar {
                    self.focus = Focus::Diff;
                }
            }
            KeyCode::Char('t') => {
                self.highlighter.cycle_theme();
            }
            KeyCode::Char('h') | KeyCode::Left => {
                if self.focus == Focus::Diff {
                    self.diff_view.h_scroll = self.diff_view.h_scroll.saturating_sub(1);
                }
            }
            KeyCode::Char('l') | KeyCode::Right => {
                if self.focus == Focus::Diff {
                    self.diff_view.h_scroll = self.diff_view.h_scroll.saturating_add(1);
                }
            }
            KeyCode::Char('L') => {
                self.line_numbers = !self.line_numbers;
            }
            KeyCode::Char('v') => {
                self.layout_mode = match self.layout_mode {
                    LayoutMode::Auto => LayoutMode::Split,
                    LayoutMode::Split => LayoutMode::Stack,
                    LayoutMode::Stack => LayoutMode::Auto,
                };
            }
            KeyCode::Tab => {
                if self.show_sidebar {
                    self.focus = match self.focus {
                        Focus::Sidebar => Focus::Diff,
                        Focus::Diff => Focus::Sidebar,
                    };
                }
            }
            KeyCode::Char('F') => {
                self.show_sidebar = true;
                self.search_state.start(SearchMode::Filename);
                self.refresh_name_matches();
                self.focus = Focus::Sidebar;
            }
            KeyCode::Char('/') => {
                self.search_state.start(SearchMode::Content);
                self.focus = Focus::Diff;
            }
            KeyCode::Char('n') => {}
            KeyCode::Char('N') => {}
            _ => {}
        }
    }

    fn refresh_name_matches(&mut self) {
        let file_names: Vec<&str> = self.files.iter().map(|f| f.path.as_str()).collect();
        self.search_state.update_filename_matches(&file_names);
    }

    fn render_menu(&self, f: &mut Frame, area: Rect) {
        let items = vec![
            (" Quit:q ", 'q'),
            (" Sidebar:s ", 's'),
            (" Layout:v ", 'v'),
            (" LineNumbers:l ", 'l'),
            (" Search:/ ", '/'),
            (" FileSearch:F ", 'F'),
        ];

        let mut spans = Vec::new();
        for (label, _) in &items {
            spans.push(Span::styled(*label, Style::default().fg(Color::Black).bg(Color::White)));
        }

        let paragraph = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::White));
        f.render_widget(paragraph, area);
    }

    fn render(&mut self, f: &mut Frame) {
        let total_area = f.area();
        let menu_height = if self.menu_bar { 1u16 } else { 0u16 };
        let status_height = 1u16;

        let menu_area = Rect {
            x: total_area.x,
            y: total_area.y,
            width: total_area.width,
            height: menu_height,
        };

        let main_area = Rect {
            x: total_area.x,
            y: total_area.y + menu_height,
            width: total_area.width,
            height: total_area.height.saturating_sub(menu_height + status_height),
        };

        let status_area = Rect {
            x: total_area.x,
            y: total_area.y + total_area.height.saturating_sub(status_height),
            width: total_area.width,
            height: status_height,
        };

        if self.menu_bar {
            self.render_menu(f, menu_area);
        }

        let show_sidebar = self.show_sidebar
            && self.layout_mode != LayoutMode::Stack
            && !self.files.is_empty();

        let constraints = if show_sidebar {
            let w = main_area.width;
            let side_w = if w > 80 {
                (w as f32 * 0.25) as u16
            } else if w > 40 {
                20
            } else {
                w / 2
            };
            vec![Constraint::Length(side_w), Constraint::Min(20)]
        } else {
            vec![Constraint::Percentage(100)]
        };

        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(main_area);

        if show_sidebar {
            self.sidebar
                .render(f, chunks[0], &self.files, &self.search_state);
        }

        let diff_area = if show_sidebar { chunks[1] } else { chunks[0] };

        let file_idx = if self.search_state.active && self.search_state.mode == SearchMode::Filename {
            self.search_state.filename_matches
                .get(self.search_state.current_match)
                .copied()
                .unwrap_or(self.sidebar.selected_index)
        } else {
            self.sidebar.selected_index
        };

        if let Some(file) = self.files.get(file_idx) {
            self.diff_view.render(
                f,
                diff_area,
                file,
                &self.search_state,
                self.line_numbers,
                &mut self.highlighter,
            );
        }

        let layout_name = match self.layout_mode {
            LayoutMode::Auto => "auto",
            LayoutMode::Split => "split",
            LayoutMode::Stack => "stack",
        };

        let focused_str = match (self.show_sidebar, self.focus) {
            (false, _) => "diff",
            (true, Focus::Sidebar) => "sidebar",
            (true, Focus::Diff) => "diff",
        };

        ui::StatusBar::render(
            f,
            status_area,
            &self.search_state,
            layout_name,
            focused_str,
            self.show_sidebar,
            &self.highlighter.theme_name(),
        );
    }
}

pub fn run_app(files: Vec<FileDiff>, watch_paths: Option<Vec<PathBuf>>) -> Result<()> {
    let mut terminal = ratatui::init();
    let mut app = App::new(files, watch_paths.clone());

    if let Some(paths) = &app.watch_paths {
        let (tx, rx) = mpsc::channel();
        app.reload_tx = Some(tx.clone());

        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            if let Ok(event) = res {
                if matches!(
                    event.kind,
                    EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
                ) {
                    let _ = tx.send(());
                }
            }
        })
        .context("Failed to create file watcher")?;

        for path in paths {
            watcher
                .watch(path, RecursiveMode::Recursive)
                .context("Failed to watch path")?;
        }

        let result = run_event_loop_with_watch(&mut terminal, &mut app, rx);

        ratatui::restore();
        result
    } else {
        let result = run_event_loop(&mut terminal, &mut app);
        ratatui::restore();
        result
    }
}

fn run_event_loop(
    terminal: &mut Terminal<impl ratatui::backend::Backend>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| app.render(f))?;

        if app.quit {
            break;
        }

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }
    }

    Ok(())
}

fn run_event_loop_with_watch(
    terminal: &mut Terminal<impl ratatui::backend::Backend>,
    app: &mut App,
    rx: mpsc::Receiver<()>,
) -> Result<()> {
    loop {
        terminal.draw(|f| app.render(f))?;

        if app.quit {
            break;
        }

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }

        if let Ok(()) = rx.try_recv() {
            if let Ok(_repo) = git2::Repository::discover(".") {
                let diff_output = crate::diff::git_diff(false, false);
                if let Ok(new_files) = diff_output {
                    let old_idx = app.sidebar.selected_index;
                    app.files = new_files;
                    app.sidebar.selected_index = old_idx.min(app.files.len().saturating_sub(1));
                    app.diff_view.scroll_offset = 0;
                    app.diff_view.h_scroll = 0;

                    if app.search_state.active
                        && app.search_state.mode == SearchMode::Filename
                    {
                        app.refresh_name_matches();
                    }
                }
            }
        }
    }

    Ok(())
}
