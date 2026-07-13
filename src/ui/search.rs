#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchMode {
    Content,
    Filename,
}

#[derive(Debug, Clone)]
pub struct SearchState {
    pub active: bool,
    pub mode: SearchMode,
    pub query: String,
    pub current_match: usize,
    pub filename_matches: Vec<usize>,
}

impl SearchState {
    pub fn new() -> Self {
        Self {
            active: false,
            mode: SearchMode::Content,
            query: String::new(),
            current_match: 0,
            filename_matches: Vec::new(),
        }
    }

    pub fn clear(&mut self) {
        self.active = false;
        self.query.clear();
        self.current_match = 0;
        self.filename_matches.clear();
    }

    pub fn start(&mut self, mode: SearchMode) {
        self.active = true;
        self.mode = mode;
        self.query.clear();
        self.current_match = 0;
        self.filename_matches.clear();
    }

    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
    }

    pub fn pop_char(&mut self) {
        self.query.pop();
    }

    pub fn update_filename_matches(&mut self, file_names: &[&str]) {
        if self.query.is_empty() {
            self.filename_matches = (0..file_names.len()).collect();
        } else {
            let lower_query = self.query.to_lowercase();
            self.filename_matches = file_names
                .iter()
                .enumerate()
                .filter(|(_, name)| name.to_lowercase().contains(&lower_query))
                .map(|(i, _)| i)
                .collect();
        }
        if self.current_match >= self.filename_matches.len() {
            self.current_match = 0;
        }
    }
}
