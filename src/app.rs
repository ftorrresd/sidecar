//! Application state, the event loop, and drawing.

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph};
use ratatui::Frame;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::external::{self, Hit};
use crate::git::{self, ChangedFile};
use crate::render;
use crate::tui::{self, Tui};

/// How often we auto-refresh the diff so it tracks the agent's edits live.
const TICK: Duration = Duration::from_millis(750);

/// Window in which a second Ctrl+C confirms exit.
const QUIT_WINDOW: Duration = Duration::from_secs(2);

/// What the preview pane is currently showing.
#[derive(Clone, Debug)]
enum View {
    /// The whole-project diff.
    Home,
    /// A single file, optionally scrolled to a line.
    File { path: PathBuf, line: Option<usize> },
}

/// Which pane `j`/`k` act on.
#[derive(Clone, Copy, PartialEq)]
enum Focus {
    Left,
    Right,
}

/// How diffs are laid out (delta rendering).
#[derive(Clone, Copy, PartialEq)]
enum DiffMode {
    /// Unified ("stacked") diff.
    Stacked,
    /// Two-column side-by-side diff.
    SideBySide,
    /// Side-by-side when the preview is wide enough, otherwise stacked.
    Auto,
}

/// Minimum preview width (columns) at which Auto switches to side-by-side.
const AUTO_SIDE_BY_SIDE_WIDTH: u16 = 120;

/// Label for the project-root row (whole-project diff).
const ROOT_LABEL: &str = "● PROJECT-ROOT";

/// A hunk located in the rendered preview.
#[derive(Clone)]
struct Hunk {
    /// Line offset of the hunk within the preview (scroll target).
    offset: u16,
    /// Repository-relative path the hunk belongs to (from the file header).
    path: Option<String>,
    /// New-file line number where the hunk starts.
    line: usize,
}

impl DiffMode {
    fn label(self) -> &'static str {
        match self {
            DiffMode::Stacked => "stacked",
            DiffMode::SideBySide => "side-by-side",
            DiffMode::Auto => "auto",
        }
    }
}

pub struct App {
    root: PathBuf,
    files: Vec<ChangedFile>,
    list_state: ListState,
    view: View,
    preview: Text<'static>,
    scroll: u16,
    preview_width: u16,
    preview_height: u16,
    needs_render: bool,
    message: String,
    focus: Focus,
    show_left: bool,
    diff_mode: DiffMode,
    /// Hunks located in the current preview, for `[`/`]` and `e`.
    hunks: Vec<Hunk>,
    /// Whether the diff auto-refreshes on the idle tick.
    auto_refresh: bool,
    /// Whether the keybinding help overlay is shown.
    show_help: bool,
    /// Whether the preview wraps long lines.
    wrap: bool,
    /// Panel rectangles from the last layout, for mouse hit-testing.
    list_area: Rect,
    preview_area: Rect,
    /// Timestamp of the last Ctrl+C, for the two-press exit.
    pending_quit: Option<Instant>,
    quit: bool,
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let files = git::changed_files(&root).unwrap_or_default();
        let mut list_state = ListState::default();
        list_state.select(Some(0)); // HOME row
        Ok(Self {
            root,
            files,
            list_state,
            view: View::Home,
            preview: Text::raw(""),
            scroll: 0,
            preview_width: 0,
            preview_height: 0,
            needs_render: true,
            message: String::new(),
            focus: Focus::Left,
            show_left: true,
            diff_mode: DiffMode::Auto,
            hunks: Vec::new(),
            auto_refresh: false,
            show_help: false,
            wrap: false,
            list_area: Rect::default(),
            preview_area: Rect::default(),
            pending_quit: None,
            quit: false,
        })
    }

    /// Main loop.
    pub fn run(&mut self, terminal: &mut Tui) -> Result<()> {
        while !self.quit {
            // Compute the layout up front so the preview is rendered at the
            // exact pane width (delta/bat need to know the wrap width).
            let size = terminal.size()?;
            let area = Rect::new(0, 0, size.width, size.height);
            let (list_a, preview, _footer) =
                layout(area, self.show_left, self.desired_left_width(area.width));
            self.list_area = list_a;
            self.preview_area = preview;
            let pw = preview.width.saturating_sub(2);
            let ph = preview.height.saturating_sub(2);
            if pw != self.preview_width {
                self.preview_width = pw;
                self.needs_render = true;
            }
            self.preview_height = ph;

            if self.needs_render {
                self.render_preview();
                self.needs_render = false;
            }

            terminal.draw(|f| self.draw(f))?;

            // With auto-refresh off we can block until the next event; with it
            // on we wake every TICK to re-read the diff.
            let got_event = if self.auto_refresh {
                event::poll(TICK)?
            } else {
                true
            };

            if got_event {
                match event::read()? {
                    Event::Key(key) if key.kind == KeyEventKind::Press => {
                        self.handle_key(key, terminal)?;
                    }
                    Event::Mouse(m) => self.handle_mouse(m),
                    _ => {}
                }
            } else {
                // Idle tick: refresh the changed-file list and the diff so the
                // view stays in sync with the coding agent's edits.
                self.reload();
            }
        }
        Ok(())
    }

    /// Rebuild the changed-file list, preserving the selected path when possible.
    fn reload(&mut self) {
        let selected_path = self.selected_changed_path();
        self.files = git::changed_files(&self.root).unwrap_or_default();

        // Restore selection by path, else clamp into range.
        let new_index = selected_path
            .and_then(|p| self.files.iter().position(|f| f.path == p))
            .map(|i| i + 1)
            .unwrap_or(0);
        self.list_state.select(Some(new_index.min(self.row_count() - 1)));
        self.needs_render = true;
    }

    /// Path of the currently selected changed file (None for HOME row).
    fn selected_changed_path(&self) -> Option<String> {
        match self.list_state.selected() {
            Some(0) | None => None,
            Some(i) => self.files.get(i - 1).map(|f| f.path.clone()),
        }
    }

    /// Number of rows in the list (HOME + one per changed file).
    fn row_count(&self) -> usize {
        self.files.len() + 1
    }

    /// Set the view from the current list selection and reset scroll.
    fn sync_view_from_list(&mut self) {
        self.view = match self.list_state.selected() {
            Some(0) | None => View::Home,
            Some(i) => match self.files.get(i - 1) {
                Some(f) => View::File {
                    path: PathBuf::from(&f.path),
                    line: None,
                },
                None => View::Home,
            },
        };
        self.scroll = 0;
        self.needs_render = true;
    }

    /// Whether the current diff mode + width resolves to side-by-side.
    fn side_by_side(&self) -> bool {
        match self.diff_mode {
            DiffMode::Stacked => false,
            DiffMode::SideBySide => true,
            DiffMode::Auto => self.preview_width >= AUTO_SIDE_BY_SIDE_WIDTH,
        }
    }

    /// Render the preview text for the current view.
    fn render_preview(&mut self) {
        let width = self.preview_width.max(20);
        let sbs = self.side_by_side();
        let wrap = self.wrap;
        let mut is_diff = true;
        // A requested target line to scroll to (applied once hunks are known).
        let target_line = match &self.view {
            View::File { line, .. } => *line,
            View::Home => None,
        };
        self.preview = match &self.view {
            View::Home => {
                let raw = git::project_diff_raw(&self.root).unwrap_or_default();
                render::diff_text(&raw, width, sbs, wrap)
            }
            View::File { path, .. } => {
                let rel = path.to_string_lossy().to_string();
                if git::is_tracked(&self.root, &rel) {
                    match git::file_has_diff(&self.root, &rel) {
                        Ok(true) => {
                            let raw = git::file_diff_raw(&self.root, &rel).unwrap_or_default();
                            render::diff_text(&raw, width, sbs, wrap)
                        }
                        _ => {
                            is_diff = false;
                            render::content_text(&self.root.join(path), width, wrap)
                        }
                    }
                } else if self.root.join(path).is_file() {
                    // Untracked file: show it as a new-file diff.
                    let raw = git::untracked_diff_raw(&self.root, &rel);
                    render::diff_text(&raw, width, sbs, wrap)
                } else {
                    is_diff = false;
                    render::content_text(&self.root.join(path), width, wrap)
                }
            }
        };
        self.hunks = if is_diff {
            analyze_hunks(&self.preview)
        } else {
            Vec::new()
        };
        if let Some(l) = target_line {
            self.scroll_to_line(l);
        }
        self.clamp_scroll();
    }

    fn clamp_scroll(&mut self) {
        let total = self.preview.lines.len() as u16;
        let max = total.saturating_sub(self.preview_height.max(1));
        if self.scroll > max {
            self.scroll = max;
        }
    }

    /// Scroll so that new-file line `l` is at the top. In a diff we align to the
    /// enclosing hunk; in plain content, bat's gutter line numbers match.
    fn scroll_to_line(&mut self, l: usize) {
        self.scroll = if self.hunks.is_empty() {
            l.saturating_sub(1).min(u16::MAX as usize) as u16
        } else {
            self.hunks
                .iter()
                .rev()
                .find(|h| h.line <= l)
                .or_else(|| self.hunks.first())
                .map(|h| h.offset)
                .unwrap_or(0)
        };
        self.clamp_scroll();
    }

    // ---- Key handling ---------------------------------------------------

    fn handle_key(&mut self, key: KeyEvent, terminal: &mut Tui) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // ---- Exit: Ctrl+D once, or Ctrl+C twice within QUIT_WINDOW ----
        if ctrl && matches!(key.code, KeyCode::Char('d') | KeyCode::Char('D')) {
            self.quit = true;
            return Ok(());
        }
        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')) {
            let now = Instant::now();
            if self
                .pending_quit
                .is_some_and(|t| now.duration_since(t) < QUIT_WINDOW)
            {
                self.quit = true;
            } else {
                self.pending_quit = Some(now);
                self.message = "Press Ctrl+C again to exit (or Ctrl+D)".into();
            }
            return Ok(());
        }
        // Any other key cancels a pending Ctrl+C and clears the status line.
        self.pending_quit = None;
        self.message.clear();

        // Help overlay: '?' toggles it; while it's up, any key dismisses it.
        if matches!(key.code, KeyCode::Char('?')) {
            self.show_help = !self.show_help;
            return Ok(());
        }
        if self.show_help {
            self.show_help = false;
            return Ok(());
        }

        let on_left = self.focus == Focus::Left && self.show_left;
        match key.code {
            // Focus movement (h has no effect while the left panel is hidden)
            KeyCode::Char('h') if self.show_left => self.focus = Focus::Left,
            KeyCode::Char('l') => self.focus = Focus::Right,

            // Toggle the left panel
            KeyCode::Char('S') => {
                self.show_left = !self.show_left;
                if !self.show_left {
                    self.focus = Focus::Right;
                }
            }

            // Toggle preview line wrapping
            KeyCode::Char('W') => {
                self.wrap = !self.wrap;
                self.needs_render = true;
                self.message = format!("wrap: {}", if self.wrap { "on" } else { "off" });
            }

            // j/k act on the focused panel
            KeyCode::Char('j') | KeyCode::Down => {
                if on_left {
                    self.select_next();
                } else {
                    self.scroll_preview(1);
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if on_left {
                    self.select_prev();
                } else {
                    self.scroll_preview(-1);
                }
            }
            KeyCode::Char('g') => {
                if on_left {
                    self.list_state.select(Some(0));
                    self.sync_view_from_list();
                } else {
                    self.scroll = 0;
                }
            }
            KeyCode::Char('G') => {
                if on_left {
                    self.list_state.select(Some(self.row_count() - 1));
                    self.sync_view_from_list();
                } else {
                    self.scroll = self.preview.lines.len() as u16;
                    self.clamp_scroll();
                }
            }

            // PROJECT-ROOT: jump straight to the project-wide diff
            KeyCode::Char('H') | KeyCode::Home => {
                self.list_state.select(Some(0));
                self.sync_view_from_list();
            }

            // Hunk navigation within the diff
            KeyCode::Char(']') => self.jump_hunk(1),
            KeyCode::Char('[') => self.jump_hunk(-1),

            // Open the current hunk's file in sidecar (not $EDITOR)
            KeyCode::Enter => self.open_current_hunk(),

            KeyCode::Char('r') => self.reload(),
            KeyCode::Char('R') => self.toggle_auto_refresh(),

            // Diff layout
            KeyCode::Char('1') => self.set_diff_mode(DiffMode::Stacked),
            KeyCode::Char('2') => self.set_diff_mode(DiffMode::SideBySide),
            KeyCode::Char('3') => self.set_diff_mode(DiffMode::Auto),

            // Preview paging (independent of focus)
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.scroll_preview(self.preview_height as i32)
            }
            KeyCode::PageUp => self.scroll_preview(-(self.preview_height as i32)),

            // External tools
            KeyCode::Char('z') => self.open_lazygit(terminal)?,
            KeyCode::Char('y') => self.open_yazi(terminal)?,
            KeyCode::Char('f') => self.pick_file(terminal)?,
            KeyCode::Char('/') => self.search(terminal, SearchScope::Project)?,
            KeyCode::Char('s') => self.search(terminal, SearchScope::File)?,
            KeyCode::Char('e') => self.open_editor(terminal)?,

            _ => {}
        }
        Ok(())
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        if self.show_help {
            return;
        }
        let over_preview = contains(self.preview_area, m.column, m.row);
        let over_list = self.show_left && contains(self.list_area, m.column, m.row);
        match m.kind {
            MouseEventKind::ScrollDown => {
                if over_list {
                    self.select_next();
                } else {
                    self.scroll_preview(3);
                }
            }
            MouseEventKind::ScrollUp => {
                if over_list {
                    self.select_prev();
                } else {
                    self.scroll_preview(-3);
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if over_list {
                    self.focus = Focus::Left;
                    if let Some(idx) = self.list_row_at(m.row) {
                        self.list_state.select(Some(idx));
                        self.sync_view_from_list();
                    }
                } else if over_preview {
                    self.focus = Focus::Right;
                }
            }
            _ => {}
        }
    }

    /// Map a screen row to a list index (accounting for the border and scroll).
    fn list_row_at(&self, row: u16) -> Option<usize> {
        let inner_top = self.list_area.y + 1; // top border
        if row < inner_top {
            return None;
        }
        let idx = self.list_state.offset() + (row - inner_top) as usize;
        (idx < self.row_count()).then_some(idx)
    }

    fn toggle_auto_refresh(&mut self) {
        self.auto_refresh = !self.auto_refresh;
        self.message = format!(
            "auto-refresh: {}",
            if self.auto_refresh { "on" } else { "off" }
        );
    }

    /// Scroll to the previous (`dir < 0`) or next (`dir > 0`) hunk.
    fn jump_hunk(&mut self, dir: i32) {
        if self.hunks.is_empty() {
            self.message = "No hunks here.".into();
            return;
        }
        let cur = self.scroll;
        let target = if dir > 0 {
            self.hunks.iter().find(|h| h.offset > cur).map(|h| h.offset)
        } else {
            self.hunks
                .iter()
                .rev()
                .find(|h| h.offset < cur)
                .map(|h| h.offset)
        };
        if let Some(offset) = target {
            self.scroll = offset;
            self.clamp_scroll();
        }
    }

    /// Open the current hunk's file *inside sidecar* (its own diff view,
    /// scrolled to the hunk). Handy from the PROJECT-ROOT diff.
    fn open_current_hunk(&mut self) {
        match self.editor_target() {
            Some((path, line)) => self.open_path(path, line),
            None => self.message = "No hunk selected.".into(),
        }
    }

    /// Open the current file (at the current hunk's line) in `$EDITOR`.
    fn open_editor(&mut self, terminal: &mut Tui) -> Result<()> {
        let Some((path, line)) = self.editor_target() else {
            self.message = "Nothing to open here.".into();
            return Ok(());
        };
        let abs = self.root.join(&path);
        let res = tui::suspend(terminal, || external::edit(&abs, line))?;
        if let Err(e) = res {
            self.message = format!("editor: {e}");
        }
        // The file may have changed while editing.
        self.reload();
        Ok(())
    }

    /// The hunk at or above the top of the preview (the "current" hunk).
    fn current_hunk(&self) -> Option<&Hunk> {
        self.hunks
            .iter()
            .rev()
            .find(|h| h.offset <= self.scroll)
            .or_else(|| self.hunks.first())
    }

    /// Resolve which file+line `e` should open, based on the view and the hunk
    /// nearest the top of the preview.
    fn editor_target(&self) -> Option<(PathBuf, Option<usize>)> {
        let anchor = self.current_hunk();
        match &self.view {
            View::File { path, .. } => Some((path.clone(), anchor.map(|h| h.line))),
            View::Home => anchor
                .and_then(|h| h.path.clone())
                .map(|p| (PathBuf::from(p), anchor.map(|h| h.line))),
        }
    }

    fn set_diff_mode(&mut self, mode: DiffMode) {
        if self.diff_mode != mode {
            self.diff_mode = mode;
            self.needs_render = true;
        }
        self.message = format!("diff: {}", mode.label());
    }

    fn select_next(&mut self) {
        let n = self.row_count();
        let cur = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some((cur + 1).min(n - 1)));
        self.sync_view_from_list();
    }

    fn select_prev(&mut self) {
        let cur = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some(cur.saturating_sub(1)));
        self.sync_view_from_list();
    }

    fn scroll_preview(&mut self, delta: i32) {
        let new = self.scroll as i32 + delta;
        self.scroll = new.max(0) as u16;
        self.clamp_scroll();
    }

    /// Point the view at an arbitrary file (from yazi/fzf/search).
    fn open_path(&mut self, path: PathBuf, line: Option<usize>) {
        let rel = path.to_string_lossy().to_string();
        // Highlight it in the list if it's a tracked change; else clear.
        match self.files.iter().position(|f| f.path == rel) {
            Some(i) => self.list_state.select(Some(i + 1)),
            None => self.list_state.select(None),
        }
        self.view = View::File { path, line };
        self.scroll = 0;
        self.needs_render = true;
    }

    fn open_lazygit(&mut self, terminal: &mut Tui) -> Result<()> {
        let root = self.root.clone();
        let res = tui::suspend(terminal, || external::open_lazygit(&root))?;
        if let Err(e) = res {
            self.message = format!("lazygit: {e}");
        }
        // Commits/staging/discards in lazygit may have changed the tree.
        self.reload();
        Ok(())
    }

    fn open_yazi(&mut self, terminal: &mut Tui) -> Result<()> {
        let root = self.root.clone();
        let picked = tui::suspend(terminal, || external::pick_with_yazi(&root))?;
        match picked {
            Ok(Some(path)) => self.open_path(path, None),
            Ok(None) => {}
            Err(e) => self.message = format!("yazi: {e}"),
        }
        Ok(())
    }

    fn pick_file(&mut self, terminal: &mut Tui) -> Result<()> {
        let root = self.root.clone();
        let picked = tui::suspend(terminal, || external::pick_file_fzf(&root))?;
        match picked {
            Ok(Some(path)) => self.open_path(path, None),
            Ok(None) => {}
            Err(e) => self.message = format!("fzf: {e}"),
        }
        Ok(())
    }

    /// Search the *diff* (added/removed lines): `/` over the whole project, `s`
    /// over the current file.
    fn search(&mut self, terminal: &mut Tui, scope: SearchScope) -> Result<()> {
        let raw = match scope {
            SearchScope::Project => git::project_diff_raw(&self.root).unwrap_or_default(),
            SearchScope::File => match &self.view {
                View::File { path, .. } => self.current_file_raw_diff(&path.to_string_lossy()),
                View::Home => {
                    self.message = "Select a file first to search its diff.".into();
                    return Ok(());
                }
            },
        };

        let index = git::index_diff(&raw);
        if index.trim().is_empty() {
            self.message = "No changed lines to search.".into();
            return Ok(());
        }

        let root = self.root.clone();
        let result: Result<Option<Hit>> =
            tui::suspend(terminal, || external::search_diff(&root, &index))?;

        match result {
            Ok(Some(hit)) => self.open_path(hit.path, hit.line),
            Ok(None) => {}
            Err(e) => self.message = format!("search: {e}"),
        }
        Ok(())
    }

    /// Raw unified diff for one file (tracked change or untracked new file).
    fn current_file_raw_diff(&self, rel: &str) -> String {
        if git::is_tracked(&self.root, rel) {
            if matches!(git::file_has_diff(&self.root, rel), Ok(true)) {
                git::file_diff_raw(&self.root, rel).unwrap_or_default()
            } else {
                String::new()
            }
        } else if self.root.join(rel).is_file() {
            git::untracked_diff_raw(&self.root, rel)
        } else {
            String::new()
        }
    }

    // ---- Drawing --------------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        let (list_area, preview_area, footer_area) =
            layout(f.area(), self.show_left, self.desired_left_width(f.area().width));
        if self.show_left {
            self.draw_list(f, list_area);
        }
        self.draw_preview(f, preview_area);
        self.draw_footer(f, footer_area);
        if self.show_help {
            self.draw_help(f);
        }
    }

    /// Left-panel width just wide enough for the longest row (clamped so the
    /// preview keeps a usable width).
    fn desired_left_width(&self, total: u16) -> u16 {
        // marker + space + path, and the PROJECT-ROOT row / list title.
        let mut content = ROOT_LABEL.chars().count();
        content = content.max(format!(" Changes ({}) ", self.files.len()).chars().count());
        for file in &self.files {
            content = content.max(2 + file.path.chars().count());
        }
        // + highlight symbol ("▶ ") + borders + a column of padding.
        let want = content as u16 + 2 + 2 + 1;
        let max = total.saturating_sub(24).max(24);
        want.clamp(18, max)
    }

    /// Border color for a panel, highlighted when it holds focus.
    fn border_style(&self, panel: Focus) -> Style {
        if self.focus == panel {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        }
    }

    fn draw_list(&mut self, f: &mut Frame, area: Rect) {
        let mut items: Vec<ListItem> = Vec::with_capacity(self.row_count());
        items.push(ListItem::new(Line::from(vec![
            Span::styled("● ", Style::default().fg(Color::Cyan)),
            Span::styled(
                "PROJECT-ROOT",
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ])));

        for file in &self.files {
            let marker = file.marker();
            let color = match marker {
                'A' | '?' => Color::Green,
                'D' => Color::Red,
                'M' => Color::Yellow,
                'R' => Color::Magenta,
                _ => Color::Gray,
            };
            items.push(ListItem::new(Line::from(vec![
                Span::styled(format!("{marker} "), Style::default().fg(color)),
                Span::raw(file.path.clone()),
            ])));
        }

        let title = format!(" Changes ({}) ", self.files.len());
        let list = List::new(items)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(self.border_style(Focus::Left))
                    .title(title),
            )
            .highlight_style(
                Style::default()
                    .bg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▶ ");

        f.render_stateful_widget(list, area, &mut self.list_state);
    }

    fn draw_preview(&mut self, f: &mut Frame, area: Rect) {
        let title = match &self.view {
            // Keep the current hunk's file visible while looping hunks.
            View::Home => match self.current_hunk().and_then(|h| h.path.as_deref()) {
                Some(path) => format!(" PROJECT-ROOT · {path} "),
                None => " PROJECT-ROOT · project-wide diff ".to_string(),
            },
            View::File { path, .. } => format!(" {} ", path.display()),
        };
        let total = self.preview.lines.len();
        let mode = self.diff_mode.label();
        let mut flags = format!("diff: {mode}");
        if self.wrap {
            flags.push_str(" · wrap");
        }
        if self.auto_refresh {
            flags.push_str(" · auto");
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.border_style(Focus::Right))
            .title(title)
            .title_bottom(format!(" {total}L · {flags} "));

        // delta/bat already wrap (or truncate) to the pane width, so the
        // paragraph just clips — no ratatui-level wrapping needed.
        let paragraph = Paragraph::new(self.preview.clone())
            .block(block)
            .scroll((self.scroll, 0));
        f.render_widget(paragraph, area);
    }

    fn draw_footer(&mut self, f: &mut Frame, area: Rect) {
        let help = "? help · h/l focus · j/k move · [ ] hunk · ⏎ open · e edit · z lazygit · / s search · ^C ^C / ^D quit";
        let line = if self.message.is_empty() {
            Line::from(Span::styled(help, Style::default().fg(Color::DarkGray)))
        } else {
            Line::from(Span::styled(
                self.message.clone(),
                Style::default().fg(Color::Yellow),
            ))
        };
        f.render_widget(Paragraph::new(line), area);
    }

    /// Centered overlay listing every keybinding.
    fn draw_help(&self, f: &mut Frame) {
        let entries: [(&str, &str); 23] = [
            ("h / l", "focus left / right panel"),
            ("j / k", "move selection (left) or scroll (right)"),
            ("g / G", "first/last item, or top/bottom of preview"),
            ("[ / ]", "previous / next hunk"),
            ("Enter", "open current hunk's file in sidecar"),
            ("PgUp/PgDn/Space", "page the preview"),
            ("H", "jump to PROJECT-ROOT (whole-project diff)"),
            ("S", "toggle the left (files) panel"),
            ("W", "toggle preview line wrapping"),
            ("1 / 2 / 3", "diff layout: stacked / side-by-side / auto"),
            ("e", "open current file/hunk in $EDITOR"),
            ("z", "open lazygit"),
            ("y", "pick a file with yazi"),
            ("f", "pick a filename with fzf"),
            ("/", "search the project diff"),
            ("s", "search the current file's diff"),
            ("r", "refresh now"),
            ("R", "toggle auto-refresh"),
            ("?", "toggle this help"),
            ("Ctrl+D", "quit"),
            ("Ctrl+C Ctrl+C", "quit"),
            ("Esc / any key", "close this help"),
            ("", ""),
        ];

        let mut lines: Vec<Line> = Vec::new();
        for (key, desc) in entries.iter() {
            if key.is_empty() {
                lines.push(Line::default());
                continue;
            }
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{key:>16}  "),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                ),
                Span::raw(*desc),
            ]));
        }

        let width = 66u16.min(f.area().width.saturating_sub(4));
        let height = (lines.len() as u16 + 2).min(f.area().height.saturating_sub(2));
        let area = centered_rect(width, height, f.area());

        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(" Keybindings ");
        f.render_widget(Clear, area);
        f.render_widget(Paragraph::new(lines).block(block), area);
    }
}

enum SearchScope {
    Project,
    File,
}

/// Split a full-screen rect into (list, preview, footer).
///
/// The list gets `left_width` columns; when `show_left` is false it collapses to
/// zero width and the preview takes the whole row.
fn layout(area: Rect, show_left: bool, left_width: u16) -> (Rect, Rect, Rect) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    if !show_left {
        let empty = Rect::new(rows[0].x, rows[0].y, 0, rows[0].height);
        return (empty, rows[0], rows[1]);
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(left_width), Constraint::Min(1)])
        .split(rows[0]);

    (cols[0], cols[1], rows[1])
}

/// Whether `(col, row)` falls inside `area`.
fn contains(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

/// A centered rect of the given size within `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

/// Locate hunks in a rendered (delta) diff.
///
/// Relies on delta's default decorations: each file section starts with its
/// path on a line followed by a solid `─` rule, and each hunk is a 3-line box
/// whose middle line begins with `<new-line-number>:`.
fn analyze_hunks(text: &Text) -> Vec<Hunk> {
    let lines: Vec<String> = text
        .lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect::<String>())
        .collect();

    let mut hunks = Vec::new();
    let mut current_path: Option<String> = None;
    for (idx, line) in lines.iter().enumerate() {
        if is_rule_line(line.trim_end()) {
            // File header: the path is on the line above the rule.
            if idx > 0 {
                let above = lines[idx - 1].trim();
                if !above.is_empty() {
                    current_path = Some(clean_path(above));
                }
            }
        } else if let Some(line_no) = parse_hunk_header(line) {
            hunks.push(Hunk {
                offset: idx.saturating_sub(1) as u16, // the box-top (─┐) line
                path: current_path.clone(),
                line: line_no,
            });
        }
    }
    hunks
}

/// A solid horizontal rule (delta's file-title underline).
fn is_rule_line(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c == '─')
}

/// Parse a delta hunk-header middle line like `36: def foo():` into `36`.
fn parse_hunk_header(line: &str) -> Option<usize> {
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() || !line[digits.len()..].starts_with(':') {
        return None;
    }
    digits.parse().ok()
}

/// Clean a delta file-header path, taking the new name across a rename arrow.
fn clean_path(s: &str) -> String {
    s.rsplit(['→', '⟶']).next().unwrap_or(s).trim().to_string()
}
