use std::collections::{HashMap, VecDeque};

use crate::{
    docker::{ComposeProject, ContainerItem, ContainerStatsSample, DockerInfo, DockerUpdate},
    events::ShutdownReason,
    ui::panel::PanelId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MainContext {
    #[default]
    Logs,
    Stats,
    Config,
    Env,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    pub running: bool,
    pub docker: DockerState,
    pub containers: Vec<ContainerItem>,
    pub projects: Vec<ComposeProject>,
    pub selected_indexes: HashMap<PanelId, usize>,
    pub active_main_context: MainContext,
    pub log_buffer: VecDeque<String>,
    pub active_stats: Option<ContainerStatsSample>,
    pub last_shutdown_reason: Option<ShutdownReason>,
    pub tick_count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DockerState {
    Unknown,
    Available(DockerInfo),
    Unavailable(String),
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            running: true,
            docker: DockerState::Unknown,
            containers: Vec::new(),
            projects: Vec::new(),
            selected_indexes: HashMap::new(),
            active_main_context: MainContext::default(),
            log_buffer: VecDeque::with_capacity(500),
            active_stats: None,
            last_shutdown_reason: None,
            tick_count: 0,
        }
    }
}

impl AppState {
    pub fn record_tick(&mut self) {
        self.tick_count = self.tick_count.saturating_add(1);
    }

    pub fn record_docker_ping(&mut self, result: Result<DockerInfo, String>) {
        self.docker = match result {
            Ok(info) => DockerState::Available(info),
            Err(message) => DockerState::Unavailable(message),
        };
    }

    pub fn apply_docker_update(&mut self, update: DockerUpdate) {
        match update {
            DockerUpdate::Connected(info) => {
                self.docker = DockerState::Available(info);
            }
            DockerUpdate::Disconnected(message) => {
                self.docker = DockerState::Unavailable(message);
            }
            DockerUpdate::Containers(containers) => {
                self.containers = containers;
                self.clamp_selections();
            }
            DockerUpdate::Event(_) => {
                // Events currently trigger container list refreshes in the supervisor,
                // so we don't need to do much here yet, but we could track last event time.
            }
        }
    }

    pub fn add_log_line(&mut self, line: String) {
        if self.log_buffer.len() >= 500 {
            self.log_buffer.pop_front();
        }
        self.log_buffer.push_back(line);
    }

    pub fn clear_log_buffer(&mut self) {
        self.log_buffer.clear();
    }

    pub fn update_stats(&mut self, stats: ContainerStatsSample) {
        self.active_stats = Some(stats);
    }

    pub fn set_main_context(&mut self, context: MainContext) {
        if self.active_main_context != context {
            self.active_main_context = context;
            self.clear_log_buffer();
            self.active_stats = None;
        }
    }

    pub fn request_shutdown(&mut self, reason: ShutdownReason) {
        self.running = false;
        self.last_shutdown_reason = Some(reason);
    }

    pub fn move_selection(&mut self, panel: PanelId, delta: isize) -> bool {
        let count = match panel {
            PanelId::Containers => self.containers.len(),
            PanelId::Projects => self.projects.len(),
            _ => return false,
        };

        if count == 0 {
            return false;
        }

        let current = self.selected_indexes.get(&panel).copied().unwrap_or(0);
        let next = if delta >= 0 {
            current.saturating_add(delta as usize).min(count - 1)
        } else {
            current.saturating_sub(delta.abs() as usize)
        };

        if next != current {
            self.selected_indexes.insert(panel, next);
            true
        } else {
            // Ensure we at least have 0 as selection if it wasn't present
            if !self.selected_indexes.contains_key(&panel) {
                self.selected_indexes.insert(panel, 0);
                true
            } else {
                false
            }
        }
    }

    pub fn get_selection(&self, panel: PanelId) -> usize {
        self.selected_indexes.get(&panel).copied().unwrap_or(0)
    }

    fn clamp_selections(&mut self) {
        if let Some(index) = self.selected_indexes.get_mut(&PanelId::Containers) {
            let count = self.containers.len();
            if count == 0 {
                *index = 0;
            } else if *index >= count {
                *index = count - 1;
            }
        }
        // ... clamp others as needed
    }

    pub fn selected_container(&self) -> Option<&ContainerItem> {
        let index = self.get_selection(PanelId::Containers);
        self.containers.get(index)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_docker_status() {
        let mut state = AppState::default();
        let info = DockerInfo {
            server_version: Some("25.0.0".to_string()),
        };

        state.record_docker_ping(Ok(info.clone()));
        assert_eq!(state.docker, DockerState::Available(info));

        state.record_docker_ping(Err("daemon unavailable".to_string()));
        assert_eq!(
            state.docker,
            DockerState::Unavailable("daemon unavailable".to_string())
        );
    }

    #[test]
    fn shutdown_flips_running_flag() {
        let mut state = AppState::default();
        state.request_shutdown(ShutdownReason::User);
        assert!(!state.running);
        assert_eq!(state.last_shutdown_reason, Some(ShutdownReason::User));
    }
}
