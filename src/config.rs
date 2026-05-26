use std::{env, fs, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub app_name: String,
    pub tick_rate_ms: u64,
    pub docker: DockerConfig,
    pub terminal: TerminalConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerConfig {
    pub command: String,
    pub ping_on_startup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    pub alternate_screen: bool,
    pub raw_mode: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("failed to read config file {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: toml::de::Error,
    },
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            app_name: "lazydocker-rs".to_string(),
            tick_rate_ms: 250,
            docker: DockerConfig::default(),
            terminal: TerminalConfig::default(),
        }
    }
}

impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            command: "docker".to_string(),
            ping_on_startup: true,
        }
    }
}

impl Default for TerminalConfig {
    fn default() -> Self {
        Self {
            alternate_screen: true,
            raw_mode: true,
        }
    }
}

impl AppConfig {
    pub fn load() -> Result<Self, ConfigError> {
        match env::var("LAZYDOCKER_RS_CONFIG") {
            Ok(path) => Self::load_from_path(path),
            Err(_) => Ok(Self::default()),
        }
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> Result<Self, ConfigError> {
        let path = path.as_ref();
        let display_path = path.display().to_string();
        let content = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: display_path.clone(),
            source,
        })?;

        toml::from_str(&content).map_err(|source| ConfigError::Parse {
            path: display_path,
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_production_ready() {
        let config = AppConfig::default();
        assert_eq!(config.app_name, "lazydocker-rs");
        assert!(config.tick_rate_ms > 0);
        assert_eq!(config.docker.command, "docker");
        assert!(config.terminal.raw_mode);
    }

    #[test]
    fn loads_partial_config_with_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
app_name = "ld"
tick_rate_ms = 500

[docker]
ping_on_startup = false
"#,
        )
        .unwrap();

        let config = AppConfig::load_from_path(&path).unwrap();
        assert_eq!(config.app_name, "ld");
        assert_eq!(config.tick_rate_ms, 500);
        assert_eq!(config.docker.command, "docker");
        assert!(!config.docker.ping_on_startup);
        assert!(config.terminal.alternate_screen);
    }
}
