use std::collections::HashMap;
use std::path::Path;

use ratatui::style::Color;
use syntect::highlighting::ThemeSet;
use syntect::parsing::{SyntaxReference, SyntaxSet};

pub const THEMES: &[&str] = &[
    "base16-ocean.dark",
    "Monokai Extended",
    "Solarized (dark)",
    "Solarized (light)",
    "GitHub",
    "Dracula",
    "OneHalfDark",
    "base16-eighties.dark",
    "base16-mocha.dark",
    "serena",
    "InspiredGitHub",
];

pub struct Highlighter {
    syntax_set: SyntaxSet,
    syntax_cache: HashMap<String, SyntaxReference>,
    theme: syntect::highlighting::Theme,
    theme_name: String,
}

impl Highlighter {
    pub fn new() -> Self {
        let syntax_set = SyntaxSet::load_defaults_newlines();
        let mut theme_set = ThemeSet::load_defaults();

        let initial_theme = "base16-ocean.dark";
        let theme = theme_set
            .themes
            .remove(initial_theme)
            .unwrap_or_else(|| {
                let first = theme_set.themes.keys().next().unwrap().clone();
                theme_set.themes.remove(&first).unwrap()
            });

        Self {
            syntax_set,
            syntax_cache: HashMap::new(),
            theme,
            theme_name: initial_theme.to_string(),
        }
    }

    pub fn theme_name(&self) -> &str {
        &self.theme_name
    }

    pub fn cycle_theme(&mut self) {
        if let Some(pos) = THEMES.iter().position(|&t| t == self.theme_name) {
            let next_idx = (pos + 1) % THEMES.len();
            self.load_theme(THEMES[next_idx]);
        }
    }

    fn load_theme(&mut self, name: &str) {
        let mut theme_set = ThemeSet::load_defaults();
        if let Some(theme) = theme_set.themes.remove(name) {
            self.theme = theme;
            self.theme_name = name.to_string();
        }
    }

    fn find_syntax(&mut self, file_path: &str) -> &SyntaxReference {
        let ext = Path::new(file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");

        if !self.syntax_cache.contains_key(ext) {
            let syntax = self
                .syntax_set
                .find_syntax_by_extension(ext)
                .unwrap_or_else(|| self.syntax_set.find_syntax_plain_text())
                .clone();
            self.syntax_cache.insert(ext.to_string(), syntax);
        }

        self.syntax_cache.get(ext).unwrap()
    }

    pub fn highlight_line(&mut self, content: &str, file_path: &str) -> Vec<(String, Color)> {
        let syntax = self.find_syntax(file_path).clone();
        let mut highlighter = syntect::easy::HighlightLines::new(&syntax, &self.theme);

        match highlighter.highlight_line(content, &self.syntax_set) {
            Ok(ranges) => ranges
                .into_iter()
                .map(|(style, text)| {
                    let color =
                        Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
                    (text.to_string(), color)
                })
                .collect(),
            Err(_) => vec![(content.to_string(), Color::White)],
        }
    }
}
