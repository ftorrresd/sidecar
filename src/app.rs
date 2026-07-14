//! Application state, the event loop, and drawing.

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use signal_hook::consts::signal::SIGTSTP;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::external;
use crate::git::{self, ChangedFile};
use crate::notes::{self, Note, SelKind};
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

/// Which set of repository files is visible in the left panel.
#[derive(Clone, Copy, PartialEq)]
enum FileListMode {
    All,
    Changes,
}

#[derive(Clone, Copy)]
enum SearchScope {
    Project,
    File,
}

impl FileListMode {
    fn label(self) -> &'static str {
        match self {
            Self::All => "All files",
            Self::Changes => "Changes",
        }
    }
}

/// Cursor + visual-selection state for the annotate (note) view.
#[derive(Clone)]
struct CursorState {
    /// 1-based file line.
    line: usize,
    /// 0-based character column into the tab-expanded line (may equal the line
    /// length, i.e. resting just past the last character).
    col: usize,
    /// Visual-selection anchor (1-based line, 0-based col); `None` unless a
    /// `v`/`V` selection is in progress.
    anchor: Option<(usize, usize)>,
    /// Character- vs line-wise selection (meaningful while `anchor` is set).
    kind: SelKind,
}

/// A note comment being typed in (a new note, or an edit of an existing one).
struct InputState {
    buffer: String,
    /// Insertion point, as a character index into `buffer`.
    cursor: usize,
    kind: SelKind,
    start: (usize, usize),
    end: (usize, usize),
    /// Id of the note being edited; `None` when composing a new one.
    editing: Option<u64>,
}

impl InputState {
    fn char_len(&self) -> usize {
        self.buffer.chars().count()
    }

    /// Byte offset of character index `idx` (or the end of the buffer).
    fn byte_at(&self, idx: usize) -> usize {
        self.buffer
            .char_indices()
            .nth(idx)
            .map(|(b, _)| b)
            .unwrap_or(self.buffer.len())
    }

    fn insert(&mut self, c: char) {
        let b = self.byte_at(self.cursor);
        self.buffer.insert(b, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let (s, e) = (self.byte_at(self.cursor - 1), self.byte_at(self.cursor));
        self.buffer.replace_range(s..e, "");
        self.cursor -= 1;
    }

    fn delete(&mut self) {
        if self.cursor >= self.char_len() {
            return;
        }
        let (s, e) = (self.byte_at(self.cursor), self.byte_at(self.cursor + 1));
        self.buffer.replace_range(s..e, "");
    }

    /// Cursor position as (line, column), both 0-based.
    fn line_col(&self) -> (usize, usize) {
        let mut line = 0;
        let mut col = 0;
        for ch in self.buffer.chars().take(self.cursor) {
            if ch == '\n' {
                line += 1;
                col = 0;
            } else {
                col += 1;
            }
        }
        (line, col)
    }

    /// Character index of (line, col), clamping col to the line's length.
    fn index_of(&self, line: usize, col: usize) -> usize {
        let mut idx = 0;
        for (li, l) in self.buffer.split('\n').enumerate() {
            let len = l.chars().count();
            if li == line {
                return idx + col.min(len);
            }
            idx += len + 1; // + newline
        }
        self.char_len()
    }

    fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }
    fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.char_len());
    }
    fn up(&mut self) {
        let (l, c) = self.line_col();
        self.cursor = if l > 0 { self.index_of(l - 1, c) } else { 0 };
    }
    fn down(&mut self) {
        let (l, c) = self.line_col();
        let n = self.buffer.split('\n').count();
        self.cursor = if l + 1 < n {
            self.index_of(l + 1, c)
        } else {
            self.char_len()
        };
    }
    fn home(&mut self) {
        let (l, _) = self.line_col();
        self.cursor = self.index_of(l, 0);
    }
    fn end(&mut self) {
        let (l, _) = self.line_col();
        self.cursor = self.index_of(l, usize::MAX);
    }
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
    all_files: Vec<String>,
    file_list_mode: FileListMode,
    show_hidden: bool,
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
    /// Anchored review notes, keyed by repo-relative path.
    notes: HashMap<String, Vec<Note>>,
    /// Whether the right pane shows the plain, note-aware annotate view.
    annotate: bool,
    /// Cursor/selection state while annotating (`Some` iff `annotate`).
    cursor: Option<CursorState>,
    /// Tab-expanded lines of the file currently shown in the annotate view.
    annotate_lines: Vec<String>,
    /// Column (within a preview line) where content begins after the gutter, for
    /// mapping mouse clicks to file columns in the annotate view.
    annotate_gutter: usize,
    /// Anchor (file line, col) of an in-progress mouse selection.
    mouse_anchor: Option<(usize, usize)>,
    /// Whether the last left-press landed on the preview pane (arms mouse drag).
    mouse_down_preview: bool,
    /// A note comment being typed in (`c`), if any.
    input: Option<InputState>,
    /// In-annotate text search: the query being typed (`/`), if active.
    find: Option<String>,
    /// The last committed in-annotate search query, for repeat searches.
    last_find: Option<String>,
    quit: bool,
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let files = git::changed_files(&root).unwrap_or_default();
        let all_files = git::all_files(&root).unwrap_or_default();
        let notes = notes::load_all(&root);
        let mut list_state = ListState::default();
        list_state.select(Some(0)); // HOME row
        let mut app = Self {
            root,
            files,
            all_files,
            file_list_mode: FileListMode::All,
            show_hidden: true,
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
            notes,
            annotate: false,
            cursor: None,
            annotate_lines: Vec::new(),
            annotate_gutter: 0,
            mouse_anchor: None,
            mouse_down_preview: false,
            input: None,
            find: None,
            last_find: None,
            quit: false,
        };
        // Drop/re-anchor notes that no longer match their file's current content.
        app.refresh_notes();
        Ok(app)
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

    /// Rebuild the file lists, preserving the selected path when possible.
    fn reload(&mut self) {
        let selected_path = self.selected_path();
        self.files = git::changed_files(&self.root).unwrap_or_default();
        self.all_files = git::all_files(&self.root).unwrap_or_default();

        // Restore selection by path, else clamp into range.
        let new_index = selected_path
            .and_then(|p| self.visible_position(&p))
            .map(|i| i + 1)
            .unwrap_or(0);
        self.list_state
            .select(Some(new_index.min(self.row_count() - 1)));
        self.refresh_notes();
        self.needs_render = true;
    }

    /// Re-anchor every note against its file's current content, dropping any that
    /// no longer match, and persist the ones that moved. Runs on each refresh so
    /// notes track the coding agent's edits.
    fn refresh_notes(&mut self) {
        let root = self.root.clone();
        let mut empties = Vec::new();
        for (rel, ns) in self.notes.iter_mut() {
            let content = std::fs::read_to_string(root.join(rel)).unwrap_or_default();
            let lines = notes::canon_lines(&content);
            let before: Vec<_> = ns.iter().map(coords).collect();
            ns.retain_mut(|n| notes::revalidate(n, &lines));
            let after: Vec<_> = ns.iter().map(coords).collect();
            if before != after {
                let _ = notes::save(&root, rel, ns);
            }
            if ns.is_empty() {
                empties.push(rel.clone());
            }
        }
        for rel in empties {
            self.notes.remove(&rel);
        }
    }

    /// Path of the currently selected file (None for HOME row).
    fn selected_path(&self) -> Option<String> {
        match self.list_state.selected() {
            Some(0) | None => None,
            Some(i) => self.visible_path(i - 1).map(str::to_owned),
        }
    }

    fn visible_file_count(&self) -> usize {
        match self.file_list_mode {
            FileListMode::All => self
                .all_files
                .iter()
                .filter(|path| self.show_hidden || !is_hidden_path(path))
                .count(),
            FileListMode::Changes => self
                .files
                .iter()
                .filter(|file| self.show_hidden || !is_hidden_path(&file.path))
                .count(),
        }
    }

    fn visible_path(&self, index: usize) -> Option<&str> {
        match self.file_list_mode {
            FileListMode::All => self
                .all_files
                .iter()
                .filter(|path| self.show_hidden || !is_hidden_path(path))
                .nth(index)
                .map(String::as_str),
            FileListMode::Changes => self
                .files
                .iter()
                .filter(|file| self.show_hidden || !is_hidden_path(&file.path))
                .nth(index)
                .map(|file| file.path.as_str()),
        }
    }

    fn visible_position(&self, path: &str) -> Option<usize> {
        match self.file_list_mode {
            FileListMode::All => self
                .all_files
                .iter()
                .filter(|file| self.show_hidden || !is_hidden_path(file))
                .position(|file| file == path),
            FileListMode::Changes => self
                .files
                .iter()
                .filter(|file| self.show_hidden || !is_hidden_path(&file.path))
                .position(|file| file.path == path),
        }
    }

    /// Number of rows in the list (HOME + one per visible file).
    fn row_count(&self) -> usize {
        self.visible_file_count() + 1
    }

    /// Set the view from the current list selection and reset scroll.
    fn sync_view_from_list(&mut self) {
        self.view = match self.list_state.selected() {
            Some(0) | None => View::Home,
            Some(i) => match self.visible_path(i - 1) {
                Some(path) => View::File {
                    path: PathBuf::from(path),
                    line: None,
                },
                None => View::Home,
            },
        };
        self.scroll = 0;
        self.reset_annotate_for_view();
        self.needs_render = true;
    }

    /// Reset cursor/input state when the previewed file changes. Files that
    /// already carry notes open straight into the annotate view so the notes are
    /// visible next to their locations; everything else opens in the diff view.
    fn reset_annotate_for_view(&mut self) {
        self.cursor = None;
        self.input = None;
        self.annotate = false;
        if let View::File { path, line } = &self.view {
            let rel = path.to_string_lossy().to_string();
            if self.notes.get(&rel).is_some_and(|n| !n.is_empty()) {
                self.annotate = true;
                self.cursor = Some(CursorState {
                    line: line.unwrap_or(1).max(1),
                    col: 0,
                    anchor: None,
                    kind: SelKind::Char,
                });
            }
        }
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
        // The annotate view has its own line-exact renderer so the cursor and
        // notes map precisely to file coordinates.
        if self.annotate {
            if let View::File { path, .. } = &self.view {
                let rel = path.to_string_lossy().to_string();
                self.render_annotate(&rel);
                return;
            }
        }
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

    /// Render the note-aware view of `rel`: one preview line per file line, over
    /// a syntax-highlighted base (via `bat`) with a line-number gutter, note
    /// markers/comments, and the cursor and any live selection overlaid.
    fn render_annotate(&mut self, rel: &str) {
        let content = std::fs::read_to_string(self.root.join(rel)).unwrap_or_default();
        self.annotate_lines = notes::canon_lines(&content);
        let total = self.annotate_lines.len();

        // Keep the cursor inside the (possibly just-changed) file.
        if let Some(cur) = &mut self.cursor {
            if total == 0 {
                cur.line = 1;
                cur.col = 0;
            } else {
                cur.line = cur.line.clamp(1, total);
                let len = self.annotate_lines[cur.line - 1].chars().count();
                cur.col = cur.col.min(len);
            }
        }

        let file_notes = self.notes.get(rel).cloned().unwrap_or_default();
        let gutter_style = Style::default().fg(Color::DarkGray);
        let comment_style = Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::ITALIC);
        let gutter_w = total.max(1).to_string().len();

        // Syntax-highlighted base; must yield at least one rendered row per file
        // line for the gutter parsing to line up, else fall back to a plain view.
        let width = self.preview_width.max(20);
        let hl = if total > 0 {
            render::highlight(&self.annotate_lines.join("\n"), rel, width)
        } else {
            Text::default()
        };
        let use_hl = total > 0 && hl.lines.len() >= total;

        let mut out: Vec<Line> = Vec::with_capacity(total.max(1));
        let mut gutter_cols = 0usize;
        for i0 in 0..total {
            let lineno = i0 + 1;
            let nchars = self.annotate_lines[i0].chars().count();
            let starts_here: Vec<&Note> = file_notes
                .iter()
                .filter(|n| n.start_line == lineno)
                .collect();

            // Base cells `(char, style)` plus the column where content starts.
            let (mut cells, content_start) = if use_hl {
                let flat = flatten_line(&hl.lines[i0]);
                let cs = gutter_end(&flat);
                (flat, cs)
            } else {
                let gutter = format!(" {lineno:>gutter_w$} │ ");
                let cs = gutter.chars().count();
                let mut cells: Vec<(char, Style)> =
                    gutter.chars().map(|c| (c, gutter_style)).collect();
                cells.extend(
                    self.annotate_lines[i0]
                        .chars()
                        .map(|c| (c, Style::default())),
                );
                (cells, cs)
            };
            gutter_cols = content_start;

            // Note marker in the leftmost gutter cell.
            if !starts_here.is_empty() {
                if let Some(first) = cells.first_mut() {
                    *first = ('●', Style::default().fg(Color::Cyan));
                }
            }

            // Overlay cursor/selection/note styling onto the content cells,
            // preserving the syntax color underneath.
            for c0 in 0..nchars {
                if let Some(ov) = self.overlay_style(lineno, c0, &file_notes) {
                    let idx = content_start + c0;
                    if idx < cells.len() {
                        cells[idx].1 = cells[idx].1.patch(ov);
                    }
                }
            }

            let mut spans: Vec<Span> = cells
                .into_iter()
                .map(|(c, st)| Span::styled(c.to_string(), st))
                .collect();

            // Cursor resting past the last character, or on an empty line.
            if self.cursor_at(lineno, nchars) {
                spans.push(Span::styled(
                    " ",
                    Style::default().add_modifier(Modifier::REVERSED),
                ));
            } else if nchars == 0 {
                if let Some(ov) = self.overlay_style(lineno, 0, &file_notes) {
                    spans.push(Span::styled(" ", ov));
                }
            }

            for n in &starts_here {
                // Multi-line notes collapse to one row here (one preview line per
                // file line keeps the cursor mapping exact); the full text shows
                // in the editor overlay.
                let text = n.text.replace('\n', " ⏎ ");
                spans.push(Span::styled(format!("  ▸ {text}"), comment_style));
            }
            out.push(Line::from(spans));
        }
        if out.is_empty() {
            out.push(Line::from(Span::styled(
                "(empty file — nothing to annotate)",
                gutter_style,
            )));
        }

        self.annotate_gutter = gutter_cols;
        self.preview = Text::from(out);
        self.hunks = Vec::new();
        self.ensure_cursor_visible();
        self.clamp_scroll();
    }

    /// Map a screen cell to a (1-based file line, 0-based file column) in the
    /// annotate view, or `None` if it falls outside the content.
    fn annotate_cell_at(&self, col: u16, row: u16) -> Option<(usize, usize)> {
        if !self.annotate {
            return None;
        }
        let a = self.preview_area;
        // Inside the bordered content region.
        if col < a.x + 1 || col >= a.x + a.width.saturating_sub(1) {
            return None;
        }
        if row < a.y + 1 || row >= a.y + a.height.saturating_sub(1) {
            return None;
        }
        let rel_row = (row - (a.y + 1)) as usize;
        let line = self.scroll as usize + rel_row + 1;
        let total = self.annotate_lines.len();
        if line == 0 || line > total {
            return None;
        }
        let rel_col = (col - (a.x + 1)) as usize;
        let fcol = rel_col.saturating_sub(self.annotate_gutter);
        let len = self.annotate_lines[line - 1].chars().count();
        Some((line, fcol.min(len)))
    }

    /// Overlay to layer onto a content cell — cursor over selection over note —
    /// or `None` to leave the cell (its syntax color) untouched.
    fn overlay_style(&self, line: usize, col: usize, file_notes: &[Note]) -> Option<Style> {
        if self.cursor_at(line, col) {
            return Some(Style::default().add_modifier(Modifier::REVERSED));
        }
        if self.in_selection(line, col) {
            return Some(Style::default().bg(Color::Blue));
        }
        if file_notes.iter().any(|n| n.contains(line, col)) {
            return Some(
                Style::default()
                    .bg(Color::Rgb(58, 58, 74))
                    .add_modifier(Modifier::UNDERLINED),
            );
        }
        None
    }

    fn cursor_at(&self, line: usize, col: usize) -> bool {
        self.cursor
            .as_ref()
            .is_some_and(|c| c.line == line && c.col == col)
    }

    /// Whether `(line, col)` is inside the live `v`/`V` selection.
    fn in_selection(&self, line: usize, col: usize) -> bool {
        let Some(cur) = &self.cursor else {
            return false;
        };
        let Some(anchor) = cur.anchor else {
            return false;
        };
        let end = (cur.line, cur.col);
        let (a, b) = if end < anchor {
            (end, anchor)
        } else {
            (anchor, end)
        };
        match cur.kind {
            SelKind::Line => line >= a.0 && line <= b.0,
            SelKind::Char => {
                let after = line > a.0 || (line == a.0 && col >= a.1);
                let before = line < b.0 || (line == b.0 && col <= b.1);
                after && before
            }
        }
    }

    /// Scroll so the cursor's line is visible in the annotate view.
    fn ensure_cursor_visible(&mut self) {
        let Some(cur) = &self.cursor else { return };
        let row = (cur.line - 1) as u16;
        let height = self.preview_height.max(1);
        if row < self.scroll {
            self.scroll = row;
        } else if row >= self.scroll + height {
            self.scroll = row - height + 1;
        }
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
        if ctrl && matches!(key.code, KeyCode::Char('z') | KeyCode::Char('Z')) {
            tui::suspend(terminal, || signal_hook::low_level::raise(SIGTSTP))??;
            return Ok(());
        }
        // Any other key cancels a pending Ctrl+C and clears the status line.
        self.pending_quit = None;
        self.message.clear();

        // While typing a note, keystrokes go to the input box.
        if self.input.is_some() {
            self.handle_input_key(key);
            return Ok(());
        }

        // While typing an in-annotate search, keystrokes go to the find prompt.
        if self.find.is_some() {
            self.handle_find_key(key);
            return Ok(());
        }

        // Help overlay: '?' toggles it; while it's up, any key dismisses it.
        if matches!(key.code, KeyCode::Char('?')) {
            self.show_help = !self.show_help;
            return Ok(());
        }
        if self.show_help {
            self.show_help = false;
            return Ok(());
        }

        // Annotation can be entered while a file is selected in either panel;
        // cursor keys are only captured once the preview has focus.
        if matches!(self.view, View::File { .. }) {
            if !self.annotate {
                match key.code {
                    KeyCode::Char('n') => {
                        self.enter_annotate(None);
                        return Ok(());
                    }
                    KeyCode::Char('v') => {
                        self.enter_annotate(Some(SelKind::Char));
                        return Ok(());
                    }
                    KeyCode::Char('V') => {
                        self.enter_annotate(Some(SelKind::Line));
                        return Ok(());
                    }
                    _ => {}
                }
            } else if self.focus == Focus::Left
                && matches!(key.code, KeyCode::Char('n' | 'v' | 'V'))
            {
                self.focus = Focus::Right;
                if !matches!(key.code, KeyCode::Char('n')) {
                    self.handle_cursor_key(key);
                }
                return Ok(());
            } else if self.focus == Focus::Right && self.handle_cursor_key(key) {
                return Ok(());
            }
        }

        let on_left = self.focus == Focus::Left && self.show_left;
        match key.code {
            // Focus movement (h has no effect while the left panel is hidden)
            KeyCode::Char('h') if self.show_left => self.focus = Focus::Left,
            KeyCode::Char('l') => self.focus = Focus::Right,

            // Toggle the left panel
            KeyCode::Char('S') => {
                self.show_left = !self.show_left;
                self.focus = if self.show_left {
                    Focus::Left
                } else {
                    Focus::Right
                };
            }

            // Toggle the files shown in the left panel.
            KeyCode::Char('C') => self.toggle_file_list_mode(),
            KeyCode::Char('.') => self.toggle_hidden_files(),

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
            KeyCode::Char('s') => self.search(terminal, SearchScope::Project)?,
            KeyCode::Char('/') => self.search(terminal, SearchScope::File)?,
            KeyCode::Char('e') => self.open_editor(terminal)?,

            _ => {}
        }
        Ok(())
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        if self.show_help || self.input.is_some() || self.find.is_some() {
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
                self.mouse_down_preview = false;
                if over_list {
                    self.focus = Focus::Left;
                    if let Some(idx) = self.list_row_at(m.row) {
                        self.list_state.select(Some(idx));
                        self.sync_view_from_list();
                    }
                } else if over_preview {
                    self.focus = Focus::Right;
                    self.mouse_down_preview = matches!(self.view, View::File { .. });
                    self.mouse_anchor = None;
                    // In the annotate view, position the cursor at the click and
                    // clear any selection; a drag from here starts a new one.
                    if let Some(cell) = self.annotate_cell_at(m.column, m.row) {
                        if let Some(cur) = &mut self.cursor {
                            cur.line = cell.0;
                            cur.col = cell.1;
                            cur.anchor = None;
                        }
                        self.mouse_anchor = Some(cell);
                        self.needs_render = true;
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if !self.mouse_down_preview {
                    return;
                }
                // A drag over a file preview selects text — entering the annotate
                // view first if the diff/content view is showing.
                if !self.annotate {
                    self.enter_annotate(None);
                    return;
                }
                if let Some(cell) = self.annotate_cell_at(m.column, m.row) {
                    if let Some(cur) = &mut self.cursor {
                        if cur.anchor.is_none() {
                            cur.anchor = Some(self.mouse_anchor.unwrap_or(cell));
                            cur.kind = SelKind::Char;
                        }
                        cur.line = cell.0;
                        cur.col = cell.1;
                    }
                    self.copy_selection();
                    self.ensure_cursor_visible();
                    self.needs_render = true;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.mouse_down_preview = false;
                self.mouse_anchor = None;
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

    fn toggle_file_list_mode(&mut self) {
        let selected_path = self.selected_path();
        self.file_list_mode = match self.file_list_mode {
            FileListMode::All => FileListMode::Changes,
            FileListMode::Changes => FileListMode::All,
        };
        let selected = selected_path
            .and_then(|path| self.visible_position(&path))
            .map(|index| index + 1)
            .unwrap_or(0);
        self.list_state.select(Some(selected));
        self.sync_view_from_list();
        self.message = format!("files: {}", self.file_list_mode.label());
    }

    fn toggle_hidden_files(&mut self) {
        let selected_path = self.selected_path();
        self.show_hidden = !self.show_hidden;
        let selected = selected_path
            .and_then(|path| self.visible_position(&path))
            .map(|index| index + 1)
            .unwrap_or(0);
        self.list_state.select(Some(selected));
        self.sync_view_from_list();
        self.message = format!(
            "hidden files: {}",
            if self.show_hidden { "shown" } else { "hidden" }
        );
    }

    // ---- Annotate view: cursor, selection, notes ------------------------

    /// Switch the right pane to the annotate view, placing the cursor at the top
    /// and optionally starting a `v`/`V` selection there.
    fn enter_annotate(&mut self, sel: Option<SelKind>) {
        self.focus = Focus::Right;
        self.annotate = true;
        self.scroll = 0;
        let (anchor, kind) = match sel {
            Some(k) => (Some((1, 0)), k),
            None => (None, SelKind::Char),
        };
        self.cursor = Some(CursorState {
            line: 1,
            col: 0,
            anchor,
            kind,
        });
        self.message = "annotate · v/V select · c comment · d delete · n/Esc exit".into();
        self.needs_render = true;
    }

    /// Leave the annotate view, back to the diff/content preview.
    fn exit_annotate(&mut self) {
        self.annotate = false;
        self.cursor = None;
        self.scroll = 0;
        self.needs_render = true;
    }

    /// Handle a keypress while the annotate cursor is active. Returns whether the
    /// key was consumed (unconsumed keys fall through to the global bindings).
    fn handle_cursor_key(&mut self, key: KeyEvent) -> bool {
        let total = self.annotate_lines.len().max(1);
        let mut consumed = true;
        let mut exit = false;
        let mut start_note = false;
        let mut delete = false;
        let mut yank = false;
        let mut start_find = false;
        {
            let Some(cur) = self.cursor.as_mut() else {
                return false;
            };
            let line_len = |line: usize| -> usize {
                self.annotate_lines
                    .get(line - 1)
                    .map(|s| s.chars().count())
                    .unwrap_or(0)
            };
            match key.code {
                KeyCode::Char('h') | KeyCode::Left => cur.col = cur.col.saturating_sub(1),
                KeyCode::Char('l') | KeyCode::Right => cur.col = cur.col.saturating_add(1),
                KeyCode::Char('j') | KeyCode::Down => cur.line = (cur.line + 1).min(total),
                KeyCode::Char('k') | KeyCode::Up => cur.line = cur.line.saturating_sub(1).max(1),
                KeyCode::Char('0') => cur.col = 0,
                KeyCode::Char('$') => cur.col = line_len(cur.line),
                KeyCode::Char('^') => cur.col = first_nonblank(&self.annotate_lines, cur.line),
                KeyCode::Char('w') => {
                    (cur.line, cur.col) = word_forward(&self.annotate_lines, cur.line, cur.col);
                }
                KeyCode::Char('b') => {
                    (cur.line, cur.col) = word_backward(&self.annotate_lines, cur.line, cur.col);
                }
                KeyCode::Char('e') => {
                    (cur.line, cur.col) = word_end(&self.annotate_lines, cur.line, cur.col);
                }
                KeyCode::Char('g') => {
                    cur.line = 1;
                    cur.col = 0;
                }
                KeyCode::Char('G') => {
                    cur.line = total;
                    cur.col = 0;
                }
                // Paragraph motions (to the surrounding blank lines).
                KeyCode::Char('{') => {
                    cur.line = para_back(&self.annotate_lines, cur.line);
                    cur.col = 0;
                }
                KeyCode::Char('}') => {
                    cur.line = para_fwd(&self.annotate_lines, cur.line);
                    cur.col = 0;
                }
                // Section motions (to unindented, top-level lines).
                KeyCode::Char('[') => {
                    cur.line = section_back(&self.annotate_lines, cur.line);
                    cur.col = 0;
                }
                KeyCode::Char(']') => {
                    cur.line = section_fwd(&self.annotate_lines, cur.line);
                    cur.col = 0;
                }
                KeyCode::Char('v') => {
                    cur.anchor = match cur.anchor {
                        Some(_) if cur.kind == SelKind::Char => None,
                        _ => Some((cur.line, cur.col)),
                    };
                    cur.kind = SelKind::Char;
                }
                KeyCode::Char('V') => {
                    cur.anchor = match cur.anchor {
                        Some(_) if cur.kind == SelKind::Line => None,
                        _ => Some((cur.line, cur.col)),
                    };
                    cur.kind = SelKind::Line;
                }
                KeyCode::Char('c') => start_note = true,
                KeyCode::Char('d') => delete = true,
                KeyCode::Char('y') => yank = true,
                KeyCode::Char('/') => start_find = true,
                KeyCode::Char('n') => exit = true,
                KeyCode::Esc => {
                    if cur.anchor.is_some() {
                        cur.anchor = None;
                    } else {
                        exit = true;
                    }
                }
                _ => consumed = false,
            }
            // Keep the column within the (possibly new) line.
            if consumed {
                cur.col = cur.col.min(line_len(cur.line));
            }
        }

        if exit {
            self.exit_annotate();
            return true;
        }
        if yank {
            match self.copy_selection() {
                Some(t) => {
                    self.message = format!("yanked {} char(s)", t.chars().count());
                    if let Some(c) = &mut self.cursor {
                        c.anchor = None;
                    }
                }
                None => self.message = "nothing selected".into(),
            }
            self.needs_render = true;
            return true;
        }
        if delete {
            self.delete_note_at_cursor();
        }
        if start_note {
            self.begin_note();
        }
        if start_find {
            self.find = Some(String::new());
            self.message.clear();
        }
        if consumed {
            // A live selection mirrors to the clipboard as it grows.
            if !start_note && !delete && self.cursor.as_ref().is_some_and(|c| c.anchor.is_some()) {
                self.copy_selection();
            }
            self.ensure_cursor_visible();
            self.needs_render = true;
        }
        consumed
    }

    /// Copy the live selection to the system clipboard, returning its text.
    fn copy_selection(&self) -> Option<String> {
        let cur = self.cursor.as_ref()?;
        let anchor = cur.anchor?;
        let text =
            notes::selection_text(cur.kind, anchor, (cur.line, cur.col), &self.annotate_lines)?;
        let _ = external::copy_to_clipboard(&text);
        Some(text)
    }

    /// Handle a keypress while the in-annotate search prompt (`/`) is open.
    fn handle_find_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => {
                self.find = None;
                self.message = "search cancelled".into();
            }
            KeyCode::Enter => {
                let typed = self.find.take().unwrap_or_default();
                // An empty query repeats the last search.
                let query = if typed.is_empty() {
                    self.last_find.clone().unwrap_or_default()
                } else {
                    typed
                };
                if query.is_empty() {
                    self.message = "no search pattern".into();
                } else {
                    self.last_find = Some(query.clone());
                    self.find_jump(&query);
                }
            }
            KeyCode::Backspace => {
                if let Some(q) = &mut self.find {
                    q.pop();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(q) = &mut self.find {
                    q.push(c);
                }
            }
            _ => {}
        }
    }

    /// Move the annotate cursor to the next case-insensitive match of `query`
    /// after the current position, wrapping around the file.
    fn find_jump(&mut self, query: &str) {
        let Some(cur) = self.cursor.as_ref() else {
            return;
        };
        let (cl, cc) = (cur.line, cur.col);
        let needle = query.to_lowercase();

        let mut first: Option<(usize, usize)> = None;
        let mut after: Option<(usize, usize)> = None;
        for (i, line) in self.annotate_lines.iter().enumerate() {
            let lineno = i + 1;
            let hay = line.to_lowercase();
            let mut byte = 0;
            while let Some(rel) = hay[byte..].find(&needle) {
                let abs = byte + rel;
                let col = hay[..abs].chars().count();
                if first.is_none() {
                    first = Some((lineno, col));
                }
                if after.is_none() && (lineno > cl || (lineno == cl && col > cc)) {
                    after = Some((lineno, col));
                }
                byte = abs + needle.len();
                if byte >= hay.len() {
                    break;
                }
            }
        }

        match after.or(first) {
            Some((l, c)) => {
                if let Some(cur) = &mut self.cursor {
                    cur.line = l;
                    cur.col = c;
                }
                self.ensure_cursor_visible();
                self.needs_render = true;
                self.message = if after.is_some() {
                    format!("/{query}")
                } else {
                    format!("/{query} (wrapped)")
                };
            }
            None => self.message = format!("not found: {query}"),
        }
    }

    /// Open the note-text input. With nothing selected and the cursor resting on
    /// an existing note, this edits that note; otherwise it composes a new note
    /// over the selection (or the current line).
    fn begin_note(&mut self) {
        let Some(cur) = self.cursor.clone() else {
            return;
        };
        let rel = match &self.view {
            View::File { path, .. } => path.to_string_lossy().to_string(),
            View::Home => return,
        };

        // Editing: no live selection, cursor over an existing note.
        if cur.anchor.is_none() {
            if let Some(note) = self
                .notes
                .get(&rel)
                .and_then(|ns| ns.iter().find(|n| n.contains(cur.line, cur.col)))
            {
                self.input = Some(InputState {
                    cursor: note.text.chars().count(),
                    buffer: note.text.clone(),
                    kind: note.kind,
                    start: (note.start_line, note.start_col),
                    end: (note.end_line, note.end_col),
                    editing: Some(note.id),
                });
                self.message.clear();
                return;
            }
        }

        // New note: over the selection, or the current line when nothing is selected.
        let (kind, start, end) = match cur.anchor {
            Some(a) => (cur.kind, a, (cur.line, cur.col)),
            None => (SelKind::Line, (cur.line, 0), (cur.line, 0)),
        };
        // The cursor may rest just past a line's last character; pin note columns
        // to real characters so the selection resolves.
        let clamp = |(l, c): (usize, usize)| -> (usize, usize) {
            let len = self
                .annotate_lines
                .get(l - 1)
                .map(|s| s.chars().count())
                .unwrap_or(0);
            (l, if len == 0 { 0 } else { c.min(len - 1) })
        };
        let (start, end) = if kind == SelKind::Char {
            (clamp(start), clamp(end))
        } else {
            (start, end)
        };
        self.input = Some(InputState {
            buffer: String::new(),
            cursor: 0,
            kind,
            start,
            end,
            editing: None,
        });
        self.message.clear();
    }

    fn handle_input_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        // Ctrl+S saves, Esc cancels — these touch `self`, so handle before
        // borrowing the input box.
        match key.code {
            KeyCode::Esc => {
                self.input = None;
                self.message = "note cancelled".into();
                return;
            }
            KeyCode::Char('s') if ctrl => {
                self.commit_note();
                return;
            }
            _ => {}
        }
        let Some(i) = &mut self.input else {
            return;
        };
        match key.code {
            // Enter inserts a newline (notes are multi-line; Ctrl+S saves).
            KeyCode::Enter => i.insert('\n'),
            KeyCode::Backspace => i.backspace(),
            KeyCode::Delete => i.delete(),
            KeyCode::Left => i.left(),
            KeyCode::Right => i.right(),
            KeyCode::Up => i.up(),
            KeyCode::Down => i.down(),
            KeyCode::Home => i.home(),
            KeyCode::End => i.end(),
            KeyCode::Char(c) if !ctrl => i.insert(c),
            _ => {}
        }
    }

    /// Save the note being composed or edited, anchoring new notes to their text.
    fn commit_note(&mut self) {
        let Some(input) = self.input.take() else {
            return;
        };
        let text = input.buffer.trim().to_string();
        if text.is_empty() {
            self.message = "note discarded (empty)".into();
            return;
        }
        let rel = match &self.view {
            View::File { path, .. } => path.to_string_lossy().to_string(),
            View::Home => return,
        };

        if let Some(id) = input.editing {
            // Update an existing note's text in place, keeping its anchor.
            if let Some(ns) = self.notes.get_mut(&rel) {
                if let Some(n) = ns.iter_mut().find(|n| n.id == id) {
                    n.text = text;
                }
                if let Err(e) = notes::save(&self.root, &rel, ns) {
                    self.message = format!("note save failed: {e}");
                } else {
                    self.message = "note updated".into();
                }
            }
        } else {
            let Some(note) = Note::new(
                input.kind,
                input.start,
                input.end,
                text,
                &self.annotate_lines,
            ) else {
                self.message = "could not anchor note".into();
                return;
            };
            let entry = self.notes.entry(rel.clone()).or_default();
            entry.push(note);
            if let Err(e) = notes::save(&self.root, &rel, entry) {
                self.message = format!("note save failed: {e}");
            } else {
                self.message = "note saved".into();
            }
        }

        if let Some(cur) = &mut self.cursor {
            cur.anchor = None;
        }
        self.needs_render = true;
    }

    /// Delete the first note covering the cursor position, if any.
    fn delete_note_at_cursor(&mut self) {
        let Some(cur) = self.cursor.clone() else {
            return;
        };
        let rel = match &self.view {
            View::File { path, .. } => path.to_string_lossy().to_string(),
            View::Home => return,
        };
        let mut deleted = false;
        let mut empty = false;
        if let Some(ns) = self.notes.get_mut(&rel) {
            if let Some(pos) = ns.iter().position(|n| n.contains(cur.line, cur.col)) {
                ns.remove(pos);
                deleted = true;
                let _ = notes::save(&self.root, &rel, ns);
                empty = ns.is_empty();
            }
        }
        if empty {
            self.notes.remove(&rel);
        }
        if deleted {
            self.message = "note deleted".into();
            self.needs_render = true;
        } else {
            self.message = "no note under cursor".into();
        }
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
        // Highlight it when it is present in the current list; else clear.
        match self.visible_position(&rel) {
            Some(i) => self.list_state.select(Some(i + 1)),
            None => self.list_state.select(None),
        }
        self.view = View::File { path, line };
        self.scroll = 0;
        self.reset_annotate_for_view();
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

    /// fzf line search over the working tree. `/` (File) always searches the
    /// current file; `s` (Project) searches the whole repository in `All` mode
    /// or just the changed files in `Changes` mode.
    fn search(&mut self, terminal: &mut Tui, scope: SearchScope) -> Result<()> {
        let (targets, prompt, header): (Vec<PathBuf>, &str, &str) = match scope {
            SearchScope::File => match &self.view {
                View::File { path, .. } => (vec![path.clone()], "file> ", "current file"),
                View::Home => {
                    self.message = "Select a file first to search it.".into();
                    return Ok(());
                }
            },
            SearchScope::Project => match self.file_list_mode {
                FileListMode::All => (Vec::new(), "project> ", "all repository lines"),
                FileListMode::Changes => {
                    let changed: Vec<PathBuf> =
                        self.files.iter().map(|f| PathBuf::from(&f.path)).collect();
                    if changed.is_empty() {
                        self.message = "No changed files to search.".into();
                        return Ok(());
                    }
                    (changed, "changed> ", "changed files")
                }
            },
        };

        let root = self.root.clone();
        let result =
            tui::suspend(terminal, || external::search_content(&root, &targets, prompt, header))?;

        match result {
            Ok(Some(hit)) => self.open_path(hit.path, hit.line),
            Ok(None) => {}
            Err(e) => self.message = format!("search: {e}"),
        }
        Ok(())
    }

    // ---- Drawing --------------------------------------------------------

    fn draw(&mut self, f: &mut Frame) {
        let (list_area, preview_area, footer_area) = layout(
            f.area(),
            self.show_left,
            self.desired_left_width(f.area().width),
        );
        if self.show_left {
            self.draw_list(f, list_area);
        }
        self.draw_preview(f, preview_area);
        self.draw_footer(f, footer_area);
        if self.show_help {
            self.draw_help(f);
        }
        if self.input.is_some() {
            self.draw_note_input(f);
        }
    }

    /// Centered overlay for composing/editing a multi-line note.
    fn draw_note_input(&self, f: &mut Frame) {
        let Some(input) = &self.input else {
            return;
        };
        let (a, b) = (input.start.0, input.end.0);
        let where_ = if a == b {
            format!("line {a}")
        } else {
            format!("lines {a}-{b}")
        };
        let verb = if input.editing.is_some() {
            "Edit note"
        } else {
            "New note"
        };

        // The buffer, split on newlines, with a block cursor at its position.
        let (crow, ccol) = input.line_col();
        let cursor = Style::default().add_modifier(Modifier::REVERSED);
        let mut lines: Vec<Line> = Vec::new();
        for (i, part) in input.buffer.split('\n').enumerate() {
            if i == crow {
                let chars: Vec<char> = part.chars().collect();
                let before: String = chars[..ccol.min(chars.len())].iter().collect();
                let (at, after) = if ccol < chars.len() {
                    (chars[ccol].to_string(), chars[ccol + 1..].iter().collect())
                } else {
                    (" ".to_string(), String::new())
                };
                lines.push(Line::from(vec![
                    Span::raw(before),
                    Span::styled(at, cursor),
                    Span::raw(after),
                ]));
            } else {
                lines.push(Line::from(part.to_string()));
            }
        }
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(
            "⏎ newline · Ctrl+S save · Esc cancel",
            Style::default().fg(Color::DarkGray),
        )));

        let width = 72u16.min(f.area().width.saturating_sub(4));
        let height = (lines.len() as u16 + 2).clamp(5, f.area().height.saturating_sub(2));
        let area = centered_rect(width, height, f.area());
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan))
            .title(format!(" {verb} · {where_} "));
        f.render_widget(Clear, area);
        f.render_widget(
            Paragraph::new(lines)
                .block(block)
                .wrap(Wrap { trim: false }),
            area,
        );
    }

    /// Left-panel width just wide enough for the longest row (clamped so the
    /// preview keeps a usable width).
    fn desired_left_width(&self, total: u16) -> u16 {
        // marker + space + path, and the PROJECT-ROOT row / list title.
        let mut content = ROOT_LABEL.chars().count();
        content = content.max(
            format!(
                " {} ({}) ",
                self.file_list_mode.label(),
                self.visible_file_count()
            )
            .chars()
            .count(),
        );
        for index in 0..self.visible_file_count() {
            let path = self.visible_path(index).unwrap_or_default();
            let badge = self
                .notes
                .get(path)
                .filter(|n| !n.is_empty())
                .map(|n| 3 + n.len().to_string().len())
                .unwrap_or(0);
            content = content.max(2 + path.chars().count() + badge);
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

        for index in 0..self.visible_file_count() {
            let path = self.visible_path(index).unwrap_or_default();
            let marker = self
                .files
                .iter()
                .find(|file| file.path == path)
                .map(ChangedFile::marker)
                .unwrap_or(' ');
            let color = match marker {
                'A' | '?' => Color::Green,
                'D' => Color::Red,
                'M' => Color::Yellow,
                'R' => Color::Magenta,
                _ => Color::Gray,
            };
            let mut spans = vec![
                Span::styled(format!("{marker} "), Style::default().fg(color)),
                Span::raw(path.to_owned()),
            ];
            if let Some(n) = self.notes.get(path) {
                if !n.is_empty() {
                    spans.push(Span::styled(
                        format!("  ✎{}", n.len()),
                        Style::default().fg(Color::Cyan),
                    ));
                }
            }
            items.push(ListItem::new(Line::from(spans)));
        }

        let title = format!(
            " {} ({}) ",
            self.file_list_mode.label(),
            self.visible_file_count()
        );
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
        let bottom = if self.annotate {
            match &self.cursor {
                Some(c) => format!(" {total}L · annotate · {}:{} ", c.line, c.col),
                None => format!(" {total}L · annotate "),
            }
        } else {
            let mut flags = format!("diff: {}", self.diff_mode.label());
            if self.wrap {
                flags.push_str(" · wrap");
            }
            if self.auto_refresh {
                flags.push_str(" · auto");
            }
            format!(" {total}L · {flags} ")
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(self.border_style(Focus::Right))
            .title(title)
            .title_bottom(bottom);

        // delta/bat already wrap (or truncate) to the pane width, so the
        // paragraph just clips — no ratatui-level wrapping needed.
        let paragraph = Paragraph::new(self.preview.clone())
            .block(block)
            .scroll((self.scroll, 0));
        f.render_widget(paragraph, area);
    }

    fn draw_footer(&mut self, f: &mut Frame, area: Rect) {
        let dim = Style::default().fg(Color::DarkGray);
        let help = "? help · C files · . hidden · h/l focus · j/k move · [ ] hunk · ⏎ open · v/n annotate · e edit · ^Z bg · ^C ^C / ^D quit";
        let annotate_help =
            "annotate · hjkl/wbe/{}[] move · / find · v/V select · y copy · c comment · d delete · n/Esc exit";
        let line = if let Some(q) = &self.find {
            Line::from(vec![
                Span::styled(
                    "/",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(q.clone()),
                Span::styled("▏", Style::default().fg(Color::Cyan)),
                Span::styled("   (⏎ next · Esc cancel)", dim),
            ])
        } else if !self.message.is_empty() {
            Line::from(Span::styled(
                self.message.clone(),
                Style::default().fg(Color::Yellow),
            ))
        } else if self.annotate {
            Line::from(Span::styled(annotate_help, dim))
        } else {
            Line::from(Span::styled(help, dim))
        };
        f.render_widget(Paragraph::new(line), area);
    }

    /// Centered overlay listing every keybinding.
    fn draw_help(&self, f: &mut Frame) {
        let entries = [
            ("h / l", "focus left / right panel"),
            ("j / k", "move selection (left) or scroll (right)"),
            ("g / G", "first/last item, or top/bottom of preview"),
            ("[ / ]", "previous / next hunk"),
            ("Enter", "open current hunk's file in sidecar"),
            ("PgUp/PgDn/Space", "page the preview"),
            ("H", "jump to PROJECT-ROOT (whole-project diff)"),
            ("C", "toggle all files / changes (all files by default)"),
            (".", "toggle hidden files (shown by default)"),
            ("S", "toggle the left (files) panel"),
            ("W", "toggle preview line wrapping"),
            ("1 / 2 / 3", "diff layout: stacked / side-by-side / auto"),
            ("", ""),
            ("n", "annotate the current file (cursor mode)"),
            (
                "v / V",
                "start char / line selection (also enters annotate)",
            ),
            (
                "hjkl w b e 0 ^ $ g G",
                "move the annotate cursor (vim motions)",
            ),
            ("{ } / [ ]", "prev/next paragraph / top-level section"),
            (
                "/ (in annotate)",
                "find text in the file (⏎ next match, wraps)",
            ),
            ("mouse drag", "select text (and copy it)"),
            ("y", "copy the selection to the clipboard"),
            (
                "c",
                "comment on the selection, or edit the note under the cursor",
            ),
            (
                "Ctrl+S / ⏎",
                "save the note / add a newline (notes are multi-line)",
            ),
            ("d", "delete the note under the cursor"),
            ("Esc / n", "clear selection / leave annotate mode"),
            ("← → ↑ ↓ in a note", "move the text cursor while editing"),
            ("", ""),
            ("e", "open current file/hunk in $EDITOR"),
            ("z / y / f", "lazygit / yazi / fzf"),
            ("s", "fzf search repo (All) or changed files (Changes)"),
            ("/", "fzf search the current file"),
            ("r / R", "refresh now / toggle auto-refresh"),
            ("?", "toggle this help"),
            ("Ctrl+Z", "suspend sidecar to the background"),
            ("Ctrl+D / Ctrl+C Ctrl+C", "quit"),
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
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
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
            .title(format!(" Keybindings · sidecar {} ", crate::VERSION));
        f.render_widget(Clear, area);
        f.render_widget(Paragraph::new(lines).block(block), area);
    }
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

/// A note's anchor coordinates, for detecting whether re-anchoring moved it.
fn coords(n: &Note) -> (usize, usize, usize, usize) {
    (n.start_line, n.start_col, n.end_line, n.end_col)
}

/// Flatten a rendered line into per-character `(char, style)` cells.
fn flatten_line(line: &Line) -> Vec<(char, Style)> {
    let mut cells = Vec::new();
    for span in &line.spans {
        let style = line.style.patch(span.style);
        for ch in span.content.chars() {
            cells.push((ch, style));
        }
    }
    cells
}

/// Column where content begins after `bat`'s `numbers` gutter — leading pad,
/// the line-number digits, and a single separating space.
fn gutter_end(cells: &[(char, Style)]) -> usize {
    let n = cells.len();
    let mut i = 0;
    while i < n && cells[i].0 == ' ' {
        i += 1;
    }
    while i < n && cells[i].0.is_ascii_digit() {
        i += 1;
    }
    if i < n && cells[i].0 == ' ' {
        i += 1;
    }
    i
}

// ---- Vim-style word motions for the annotate cursor -------------------------
//
// Positions are (1-based line, 0-based col). Characters split into three
// classes — blank, word (alphanumeric/`_`), and punctuation — matching vim's
// small-word (`w`/`b`/`e`) behavior closely enough for review navigation.

/// Character count of `line` (1-based) in `lines`.
fn nchars(lines: &[String], line: usize) -> usize {
    line.checked_sub(1)
        .and_then(|i| lines.get(i))
        .map(|s| s.chars().count())
        .unwrap_or(0)
}

fn char_at(lines: &[String], line: usize, col: usize) -> Option<char> {
    lines.get(line.checked_sub(1)?)?.chars().nth(col)
}

/// 0 = blank/out-of-bounds, 1 = word char, 2 = punctuation.
fn class_of(c: Option<char>) -> u8 {
    match c {
        Some(c) if c.is_whitespace() => 0,
        Some(c) if c.is_alphanumeric() || c == '_' => 1,
        Some(_) => 2,
        None => 0,
    }
}

/// The next position after `(line, col)`, rolling onto the next line's start.
fn adv(lines: &[String], line: usize, col: usize) -> Option<(usize, usize)> {
    if col + 1 < nchars(lines, line) {
        Some((line, col + 1))
    } else if line < lines.len() {
        Some((line + 1, 0))
    } else {
        None
    }
}

/// The previous position before `(line, col)`, rolling onto the prior line's end.
fn retr(lines: &[String], line: usize, col: usize) -> Option<(usize, usize)> {
    if col > 0 {
        Some((line, col - 1))
    } else if line > 1 {
        Some((line - 1, nchars(lines, line - 1).saturating_sub(1)))
    } else {
        None
    }
}

/// Whether `line` (1-based) is empty or all whitespace.
fn is_blank(lines: &[String], line: usize) -> bool {
    line.checked_sub(1)
        .and_then(|i| lines.get(i))
        .map(|s| s.trim().is_empty())
        .unwrap_or(true)
}

/// Whether `line` (1-based) starts a top-level construct (a non-blank line whose
/// first character is not whitespace).
fn is_section(lines: &[String], line: usize) -> bool {
    line.checked_sub(1)
        .and_then(|i| lines.get(i))
        .is_some_and(|s| s.chars().next().is_some_and(|c| !c.is_whitespace()))
}

/// `{` — the blank line above the current paragraph (or the first line).
fn para_back(lines: &[String], line: usize) -> usize {
    let mut l = line.saturating_sub(1);
    while l > 1 && is_blank(lines, l) {
        l -= 1;
    }
    while l > 1 && !is_blank(lines, l) {
        l -= 1;
    }
    l.max(1)
}

/// `}` — the blank line below the current paragraph (or the last line).
fn para_fwd(lines: &[String], line: usize) -> usize {
    let n = lines.len();
    if n == 0 {
        return 1;
    }
    let mut l = (line + 1).min(n);
    while l < n && is_blank(lines, l) {
        l += 1;
    }
    while l < n && !is_blank(lines, l) {
        l += 1;
    }
    l
}

/// `[` — the previous top-level (unindented) line, or the first line.
fn section_back(lines: &[String], line: usize) -> usize {
    let mut l = line.saturating_sub(1);
    while l > 1 {
        if is_section(lines, l) {
            return l;
        }
        l -= 1;
    }
    1
}

/// `]` — the next top-level (unindented) line, or the last line.
fn section_fwd(lines: &[String], line: usize) -> usize {
    let n = lines.len().max(1);
    let mut l = line + 1;
    while l < n {
        if is_section(lines, l) {
            return l;
        }
        l += 1;
    }
    n
}

/// Column of the first non-blank character on `line`, or 0.
fn first_nonblank(lines: &[String], line: usize) -> usize {
    line.checked_sub(1)
        .and_then(|i| lines.get(i))
        .and_then(|s| s.chars().position(|c| !c.is_whitespace()))
        .unwrap_or(0)
}

/// `w` — start of the next word.
fn word_forward(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    let start = class_of(char_at(lines, line, col));
    let Some(mut p) = adv(lines, line, col) else {
        return (line, col);
    };
    // Skip the rest of the current word, then any blanks.
    if start != 0 {
        while class_of(char_at(lines, p.0, p.1)) == start {
            match adv(lines, p.0, p.1) {
                Some(x) => p = x,
                None => return p,
            }
        }
    }
    while class_of(char_at(lines, p.0, p.1)) == 0 {
        match adv(lines, p.0, p.1) {
            Some(x) => p = x,
            None => break,
        }
    }
    p
}

/// `b` — start of the current or previous word.
fn word_backward(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    let Some(mut p) = retr(lines, line, col) else {
        return (line, col);
    };
    while class_of(char_at(lines, p.0, p.1)) == 0 {
        match retr(lines, p.0, p.1) {
            Some(x) => p = x,
            None => return p,
        }
    }
    let cls = class_of(char_at(lines, p.0, p.1));
    while let Some(prev) = retr(lines, p.0, p.1) {
        if class_of(char_at(lines, prev.0, prev.1)) == cls {
            p = prev;
        } else {
            break;
        }
    }
    p
}

/// `e` — end of the current or next word.
fn word_end(lines: &[String], line: usize, col: usize) -> (usize, usize) {
    let Some(mut p) = adv(lines, line, col) else {
        return (line, col);
    };
    while class_of(char_at(lines, p.0, p.1)) == 0 {
        match adv(lines, p.0, p.1) {
            Some(x) => p = x,
            None => return p,
        }
    }
    let cls = class_of(char_at(lines, p.0, p.1));
    while let Some(nx) = adv(lines, p.0, p.1) {
        if class_of(char_at(lines, nx.0, nx.1)) == cls {
            p = nx;
        } else {
            break;
        }
    }
    p
}

/// Whether `(col, row)` falls inside `area`.
fn contains(area: Rect, col: u16, row: u16) -> bool {
    col >= area.x && col < area.x + area.width && row >= area.y && row < area.y + area.height
}

fn is_hidden_path(path: &str) -> bool {
    path.split('/').any(|part| part.starts_with('.'))
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
        .map(|l| {
            l.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
        })
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

#[cfg(test)]
mod tests {
    use super::is_hidden_path;

    #[test]
    fn hidden_path_detects_dot_prefixed_components() {
        assert!(is_hidden_path(".env"));
        assert!(is_hidden_path(".github/workflows/release.yml"));
        assert!(is_hidden_path("src/.generated/file.rs"));
        assert!(!is_hidden_path("src/main.rs"));
        assert!(!is_hidden_path("src/file.test.rs"));
    }
}
