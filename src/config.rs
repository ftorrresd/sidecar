use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Default, Clone)]
pub struct Config {
    pub theme: Option<String>,
    pub mode: Option<String>,
    pub vcs: Option<String>,
    pub watch: Option<bool>,
    pub exclude_untracked: Option<bool>,
    pub line_numbers: Option<bool>,
    pub wrap_lines: Option<bool>,
    pub menu_bar: Option<bool>,
    pub agent_notes: Option<bool>,
    pub transparent_background: Option<bool>,
}

impl Config {
    pub fn load() -> Self {
        let mut config = Config::default();

        if let Some(config_dir) = dirs::config_dir() {
            let config_path = config_dir.join("runk").join("config.toml");
            if config_path.exists() {
                if let Ok(content) = std::fs::read_to_string(&config_path) {
                    if let Ok(cfg) = toml::from_str::<Config>(&content) {
                        config = cfg;
                    }
                }
            }
        }

        if let Ok(cwd) = std::env::current_dir() {
            let local_config = cwd.join(".runk").join("config.toml");
            if local_config.exists() {
                if let Ok(content) = std::fs::read_to_string(&local_config) {
                    if let Ok(cfg) = toml::from_str::<Config>(&content) {
                        if cfg.theme.is_some() {
                            config.theme = cfg.theme;
                        }
                        if cfg.mode.is_some() {
                            config.mode = cfg.mode;
                        }
                        if cfg.line_numbers.is_some() {
                            config.line_numbers = cfg.line_numbers;
                        }
                        if cfg.wrap_lines.is_some() {
                            config.wrap_lines = cfg.wrap_lines;
                        }
                        if cfg.menu_bar.is_some() {
                            config.menu_bar = cfg.menu_bar;
                        }
                    }
                }
            }
        }

        config
    }
}
