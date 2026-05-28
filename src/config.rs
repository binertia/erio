use std::{env, fs, path::Path};

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub app_name: String,
    pub tick_rate_ms: u64,
    pub log_buffer_lines: usize,
    pub log_buffer_max_bytes: usize,
    pub compact_mode_width: u16,
    pub docker: DockerConfig,
    pub terminal: TerminalConfig,
    pub custom_commands: CustomCommandsConfig,
    pub confirm_on_quit: bool,
    pub scroll_past_bottom: bool,
    pub ignore: Vec<String>,
    pub theme: ThemeConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DockerConfig {
    pub command: String,
    pub compose_binary: String,
    pub ping_on_startup: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct TerminalConfig {
    pub alternate_screen: bool,
    pub raw_mode: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomCommandsConfig {
    pub containers: Vec<CustomCommand>,
    pub images: Vec<CustomCommand>,
    pub volumes: Vec<CustomCommand>,
    pub networks: Vec<CustomCommand>,
    pub services: Vec<CustomCommand>,
    pub projects: Vec<CustomCommand>,
    pub global: Vec<CustomCommand>,
    pub bulk_containers: Vec<CustomCommand>,
    pub bulk_images: Vec<CustomCommand>,
    pub bulk_volumes: Vec<CustomCommand>,
    pub bulk_networks: Vec<CustomCommand>,
    pub bulk_services: Vec<CustomCommand>,
    pub bulk_projects: Vec<CustomCommand>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomCommand {
    pub name: String,
    pub command: String,
}

/// Escape a string for safe use inside a POSIX shell single-quoted string.
/// Wraps the value in single quotes, replacing any embedded `'` with `'\''`.
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl CustomCommand {
    /// Substitute template variables of the form `{{ .Resource.Field }}`
    /// with the provided values, shell-escaping each substitution to prevent
    /// command injection from untrusted container names or IDs.
    pub fn render(&self, vars: &TemplateVars) -> String {
        let mut result = self.command.clone();
        if let Some(id) = &vars.container_id {
            result = result.replace("{{ .Container.ID }}", &shell_escape(id));
        }
        if let Some(name) = &vars.container_name {
            result = result.replace("{{ .Container.Name }}", &shell_escape(name));
        }
        if let Some(id) = &vars.image_id {
            result = result.replace("{{ .Image.ID }}", &shell_escape(id));
        }
        if let Some(name) = &vars.image_name {
            result = result.replace("{{ .Image.Name }}", &shell_escape(name));
        }
        if let Some(name) = &vars.volume_name {
            result = result.replace("{{ .Volume.Name }}", &shell_escape(name));
        }
        if let Some(id) = &vars.network_id {
            result = result.replace("{{ .Network.ID }}", &shell_escape(id));
        }
        if let Some(name) = &vars.network_name {
            result = result.replace("{{ .Network.Name }}", &shell_escape(name));
        }
        if let Some(name) = &vars.service_name {
            result = result.replace("{{ .Service.Name }}", &shell_escape(name));
        }
        if let Some(name) = &vars.project_name {
            result = result.replace("{{ .Project.Name }}", &shell_escape(name));
        }
        result
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeConfig {
    pub border_color: String,
    pub selection_color: String,
    pub status_color: String,
    pub error_color: String,
}

impl Default for ThemeConfig {
    fn default() -> Self {
        Self {
            border_color: "blue".to_string(),
            selection_color: "yellow".to_string(),
            status_color: "cyan".to_string(),
            error_color: "red".to_string(),
        }
    }
}

/// Values available for template substitution in custom commands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TemplateVars {
    pub container_id: Option<String>,
    pub container_name: Option<String>,
    pub image_id: Option<String>,
    pub image_name: Option<String>,
    pub volume_name: Option<String>,
    pub network_id: Option<String>,
    pub network_name: Option<String>,
    pub service_name: Option<String>,
    pub project_name: Option<String>,
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
            app_name: "erio".to_string(),
            tick_rate_ms: 250,
            log_buffer_lines: 500,
            log_buffer_max_bytes: 1_000_000,
            compact_mode_width: 70,
            docker: DockerConfig::default(),
            terminal: TerminalConfig::default(),
            custom_commands: CustomCommandsConfig::default(),
            confirm_on_quit: false,
            scroll_past_bottom: false,
            ignore: Vec::new(),
            theme: ThemeConfig::default(),
        }
    }
}



impl Default for DockerConfig {
    fn default() -> Self {
        Self {
            command: "docker".to_string(),
            compose_binary: "docker compose".to_string(),
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
        if let Ok(path) = env::var("ERIO_CONFIG") {
            return Self::load_from_path(path);
        }

        if let Ok(home) = env::var("HOME") {
            let home = std::path::PathBuf::from(home);
            let xdg_path = home.join(".config").join("erio").join("config.toml");
            if xdg_path.exists() {
                return Self::load_from_path(xdg_path);
            }
            let dotfile_path = home.join(".erio").join("config.toml");
            if dotfile_path.exists() {
                return Self::load_from_path(dotfile_path);
            }
        }

        Ok(Self::default())
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
        assert_eq!(config.app_name, "erio");
        assert!(config.tick_rate_ms > 0);
        assert_eq!(config.log_buffer_lines, 500);
        assert_eq!(config.compact_mode_width, 70);
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

    #[test]
    fn loads_custom_commands_from_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[[custom_commands.containers]]
name = "Shell"
command = "docker exec -it {{ .Container.ID }} sh"

[[custom_commands.global]]
name = "Prune all"
command = "docker system prune -f"
"#,
        )
        .unwrap();

        let config = AppConfig::load_from_path(&path).unwrap();
        assert_eq!(config.custom_commands.containers.len(), 1);
        assert_eq!(config.custom_commands.containers[0].name, "Shell");
        assert_eq!(
            config.custom_commands.containers[0].command,
            "docker exec -it {{ .Container.ID }} sh"
        );
        assert_eq!(config.custom_commands.global.len(), 1);
        assert_eq!(config.custom_commands.global[0].name, "Prune all");
    }

    #[test]
    fn custom_command_renders_template_vars() {
        let cmd = CustomCommand {
            name: "Shell".to_string(),
            command: "docker exec -it {{ .Container.ID }} {{ .Container.Name }} {{ .Image.ID }} sh".to_string(),
        };
        let vars = TemplateVars {
            container_id: Some("abc123".to_string()),
            container_name: Some("web".to_string()),
            image_id: Some("img456".to_string()),
            ..Default::default()
        };
        assert_eq!(
            cmd.render(&vars),
            "docker exec -it 'abc123' 'web' 'img456' sh"
        );
    }

    #[test]
    fn custom_command_escapes_shell_metacharacters() {
        let cmd = CustomCommand {
            name: "Shell".to_string(),
            command: "docker exec -it {{ .Container.Name }} sh".to_string(),
        };
        let vars = TemplateVars {
            container_name: Some("foo; rm -rf /".to_string()),
            ..Default::default()
        };
        assert_eq!(
            cmd.render(&vars),
            "docker exec -it 'foo; rm -rf /' sh"
        );
    }

    #[test]
    fn custom_command_escapes_single_quotes() {
        let cmd = CustomCommand {
            name: "Shell".to_string(),
            command: "echo {{ .Container.Name }}".to_string(),
        };
        let vars = TemplateVars {
            container_name: Some("it's".to_string()),
            ..Default::default()
        };
        assert_eq!(
            cmd.render(&vars),
            "echo 'it'\\''s'"
        );
    }

    #[test]
    fn custom_command_leaves_unknown_templates_intact() {
        let cmd = CustomCommand {
            name: "Test".to_string(),
            command: "echo {{ .Unknown.Var }}".to_string(),
        };
        assert_eq!(cmd.render(&TemplateVars::default()), "echo {{ .Unknown.Var }}");
    }

    #[test]
    fn load_falls_back_to_xdg_config_path() {
        let dir = tempfile::tempdir().unwrap();
        let xdg_dir = dir.path().join(".config").join("erio");
        fs::create_dir_all(&xdg_dir).unwrap();
        let path = xdg_dir.join("config.toml");
        fs::write(&path, r#"app_name = "xdg-test""#).unwrap();

        let original_home = env::var_os("HOME");
        let original_env = env::var_os("ERIO_CONFIG");
        unsafe {
            env::remove_var("ERIO_CONFIG");
            env::set_var("HOME", dir.path());
        }

        let config = AppConfig::load().unwrap();
        assert_eq!(config.app_name, "xdg-test");

        unsafe {
            if let Some(home) = original_home {
                env::set_var("HOME", home);
            } else {
                env::remove_var("HOME");
            }
            if let Some(env) = original_env {
                env::set_var("ERIO_CONFIG", env);
            }
        }
    }

    #[test]
    fn load_prefers_env_var_over_xdg() {
        let dir = tempfile::tempdir().unwrap();
        let xdg_dir = dir.path().join(".config").join("erio");
        fs::create_dir_all(&xdg_dir).unwrap();
        fs::write(xdg_dir.join("config.toml"), r#"app_name = "xdg""#).unwrap();

        let env_path = dir.path().join("env-config.toml");
        fs::write(&env_path, r#"app_name = "env""#).unwrap();

        let original_home = env::var_os("HOME");
        let original_env = env::var_os("ERIO_CONFIG");
        unsafe {
            env::set_var("HOME", dir.path());
            env::set_var("ERIO_CONFIG", &env_path);
        }

        let config = AppConfig::load().unwrap();
        assert_eq!(config.app_name, "env");

        unsafe {
            if let Some(home) = original_home {
                env::set_var("HOME", home);
            } else {
                env::remove_var("HOME");
            }
            if let Some(env) = original_env {
                env::set_var("ERIO_CONFIG", env);
            } else {
                env::remove_var("ERIO_CONFIG");
            }
        }
    }
}
