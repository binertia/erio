use std::borrow::Cow;

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph},
};

use crate::state::{AppState, DockerState, MainContext};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelId {
    Projects,
    Services,
    Containers,
    Images,
    Volumes,
    Networks,
    Main,
    Status,
}

#[derive(Debug, Clone)]
pub struct RenderContext<'a> {
    pub state: &'a AppState,
    pub focused: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PanelSpec {
    id: PanelId,
    focusable: bool,
    title: &'static str,
}

impl PanelSpec {
    pub const fn new(id: PanelId, focusable: bool, title: &'static str) -> Self {
        Self {
            id,
            focusable,
            title,
        }
    }

    pub fn id(&self) -> PanelId {
        self.id
    }

    pub fn focusable(&self) -> bool {
        self.focusable
    }

    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, context: RenderContext<'_>) {
        let border_style = if context.focused {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let content = match self.id {
            PanelId::Projects => Cow::Borrowed("project discovery pending"),
            PanelId::Services => Cow::Borrowed("compose services pending"),
            PanelId::Containers => Cow::Owned(render_container_list(
                &context.state.containers,
                context.state.get_selection(PanelId::Containers),
            )),
            PanelId::Images => Cow::Borrowed("image list pending"),
            PanelId::Volumes => Cow::Borrowed("volume list pending"),
            PanelId::Networks => Cow::Borrowed("network list pending"),
            PanelId::Main => Cow::Owned(render_main_panel(context.state)),
            PanelId::Status => Cow::Owned(status_content(context.state)),
        };

        let block = Block::default()
            .title(self.title)
            .borders(Borders::ALL)
            .border_style(border_style);
        frame.render_widget(Paragraph::new(content.as_ref()).block(block), area);
    }
}

fn render_main_panel(state: &AppState) -> String {
    match state.active_main_context {
        MainContext::Logs => {
            if state.log_buffer.is_empty() {
                "Waiting for logs...".to_string()
            } else {
                state.log_buffer.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")
            }
        }
        MainContext::Stats => {
            if let Some(stats) = &state.active_stats {
                let mut output = String::new();
                output.push_str(&format!("CPU:    {:.2}%\n", stats.cpu_percent));
                output.push_str(&format!(
                    "Memory: {} / {}\n",
                    format_bytes(stats.memory_usage),
                    format_bytes(stats.memory_limit)
                ));
                output
            } else {
                "Waiting for stats...".to_string()
            }
        }
        MainContext::Config => {
            if let Some(container) = state.selected_container() {
                format!("Config for {}:\n\nID: {}\nImage: {}\n", container.names.first().cloned().unwrap_or_default(), container.id, container.image)
            } else {
                "No container selected".to_string()
            }
        }
        MainContext::Env => "Env vars pending".to_string(),
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.2} KB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.2} MB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.2} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[derive(Default)]
pub struct PanelRegistry {
    panels: Vec<PanelSpec>,
}

impl std::fmt::Debug for PanelRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PanelRegistry")
            .field("panel_count", &self.panels.len())
            .finish()
    }
}

impl PanelRegistry {
    pub fn new(panels: Vec<PanelSpec>) -> Self {
        Self { panels }
    }

    pub fn focusable_ids(&self) -> Vec<PanelId> {
        self.panels
            .iter()
            .filter(|panel| panel.focusable())
            .map(|panel| panel.id())
            .collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &PanelSpec> {
        self.panels.iter()
    }
}

pub fn build_core_panels() -> PanelRegistry {
    PanelRegistry::new(vec![
        PanelSpec::new(PanelId::Projects, true, "[1] Projects"),
        PanelSpec::new(PanelId::Services, true, "[2] Services"),
        PanelSpec::new(PanelId::Containers, true, "[3] Containers"),
        PanelSpec::new(PanelId::Images, true, "[4] Images"),
        PanelSpec::new(PanelId::Volumes, true, "[5] Volumes"),
        PanelSpec::new(PanelId::Networks, true, "[6] Networks"),
        PanelSpec::new(PanelId::Main, true, "Main"),
        PanelSpec::new(PanelId::Status, false, "Status"),
    ])
}

fn status_content(state: &AppState) -> String {
    let docker = match &state.docker {
        DockerState::Unknown => "docker: unknown".to_string(),
        DockerState::Available(info) => {
            let version = info.server_version.as_deref().unwrap_or("unknown");
            format!("docker: available ({version}) | {} containers", state.containers.len())
        }
        DockerState::Unavailable(message) => format!("docker: unavailable ({message})"),
    };

    format!("{docker} | q/Ctrl-C quit | h/l/tab focus | 1-6 jump")
}

fn render_container_list(containers: &[crate::docker::ContainerItem], selected_index: usize) -> String {
    if containers.is_empty() {
        return "No containers found".to_string();
    }

    let mut output = String::new();
    output.push_str(&format!(
        "  {:<30} {:<30} {:<20}\n",
        "NAME", "IMAGE", "STATUS"
    ));
    output.push_str(&format!(
        "  {:-<30} {:-<30} {:-<20}\n",
        "", "", ""
    ));

    for (i, container) in containers.iter().enumerate() {
        let prefix = if i == selected_index { "> " } else { "  " };
        
        let name = container.names.first().cloned().unwrap_or_default();
        let name = if name.len() > 28 { format!("{}..", &name[..26]) } else { name };
        
        let image = if container.image.len() > 28 {
            format!("{}..", &container.image[..26])
        } else {
            container.image.clone()
        };

        let status = container.status.as_deref().unwrap_or("-");
        let status = if status.len() > 18 { format!("{}..", &status[..16]) } else { status.to_string() };

        output.push_str(&format!(
            "{}{:<30} {:<30} {:<20}\n",
            prefix, name, image, status
        ));
    }

    output
}
