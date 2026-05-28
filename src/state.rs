use std::collections::{HashMap, VecDeque};

use crate::{
    docker::{
        ComposeProject, ContainerItem, ContainerStatsSample, DockerInfo, DockerUpdate, ImageItem,
        LogStream, NetworkItem, VolumeItem,
    },
    events::{DockerAction, ShutdownReason},
    ui::panel::PanelId,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MainContext {
    #[default]
    Logs,
    Stats,
    Config,
    Env,
    ImageInfo,
    VolumeInfo,
    NetworkInfo,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    #[default]
    Normal,
    Search,
    Confirm,
    Help,
    Menu,
    Leader,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AppState {
    pub running: bool,
    pub docker: DockerState,
    pub containers: Vec<ContainerItem>,
    pub images: Vec<ImageItem>,
    pub volumes: Vec<VolumeItem>,
    pub networks: Vec<NetworkItem>,
    pub projects: Vec<ComposeProject>,
    pub selected_indexes: HashMap<PanelId, usize>,
    pub active_main_context: MainContext,
    pub input_mode: InputMode,
    pub compact_sidebar_panel: PanelId,
    pub terminal_size: (u16, u16),
    pub compact_mode_width: u16,
    pub log_filter: Option<String>,
    pub log_buffer: VecDeque<(String, LogStream)>,
    pub log_buffer_capacity: usize,
    pub log_buffer_max_bytes: usize,
    /// Accumulates partial log lines across chunks that don't end with a newline.
    pub log_partial_line: Option<(String, LogStream)>,
    pub active_stats: Option<ContainerStatsSample>,
    pub stats_history: VecDeque<f64>,
    pub memory_history: VecDeque<f64>,
    pub stats_history_capacity: usize,
    pub env_vars: Vec<(String, String)>,
    pub main_scroll_offsets: HashMap<MainContext, usize>,
    pub horizontal_scroll_offsets: HashMap<MainContext, usize>,
    pub logs_follow_bottom: bool,
    pub pending_confirmation: Option<(DockerAction, String)>,
    pub pending_leader_action: Option<crate::ui::input::InputAction>,
    pub last_shutdown_reason: Option<ShutdownReason>,
    pub tick_count: u64,
    pub status_message: Option<(String, u64)>, // (message, tick_at_set)
    pub error_message: Option<String>,
    pub panel_filters: HashMap<PanelId, String>,
    pub active_filter_panel: Option<PanelId>,
    pub hide_stopped_containers: bool,
    pub menu_title: String,
    pub menu_items: Vec<String>,
    pub menu_actions: Vec<crate::events::DockerAction>,
    pub menu_selected: usize,
    pub external_command: Option<ExternalCommand>,
    pub scroll_past_bottom: bool,
    pub screen_mode: Option<crate::ui::layout::LayoutMode>,
    pub ignore_patterns: Vec<String>,
    pub theme_border: ratatui::style::Color,
    pub theme_selection: ratatui::style::Color,
    pub theme_status: ratatui::style::Color,
    pub theme_error: ratatui::style::Color,
    pub focused_panel: crate::ui::panel::PanelId,
    /// Cached filtered indices to avoid recomputing every frame.
    pub(crate) cached_filtered_indices: std::collections::HashMap<PanelId, Vec<usize>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalCommand {
    ExecShell(String),
    AttachContainer(String),
    CustomCommand { name: String, command: String },
    EditConfig(String),
    OpenConfig(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DockerState {
    Unknown,
    Available(DockerInfo),
    Unavailable(String),
}

impl DockerState {
    pub fn is_available(&self) -> bool {
        matches!(self, DockerState::Available(_))
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            running: true,
            docker: DockerState::Unknown,
            containers: Vec::new(),
            images: Vec::new(),
            volumes: Vec::new(),
            networks: Vec::new(),
            projects: Vec::new(),
            selected_indexes: HashMap::new(),
            active_main_context: MainContext::default(),
            input_mode: InputMode::default(),
            compact_sidebar_panel: PanelId::Containers,
            terminal_size: (0, 0),
            compact_mode_width: 70,
            log_filter: None,
            log_buffer: VecDeque::with_capacity(500),
            log_buffer_capacity: 500,
            log_buffer_max_bytes: 1_000_000,
            log_partial_line: None,
            active_stats: None,
            stats_history: VecDeque::with_capacity(60),
            memory_history: VecDeque::with_capacity(60),
            stats_history_capacity: 60,
            env_vars: Vec::new(),
            main_scroll_offsets: HashMap::new(),
            horizontal_scroll_offsets: HashMap::new(),
            logs_follow_bottom: true,
            pending_confirmation: None,
            pending_leader_action: None,
            last_shutdown_reason: None,
            tick_count: 0,
            status_message: None,
            error_message: None,
            panel_filters: HashMap::new(),
            active_filter_panel: None,
            hide_stopped_containers: false,
            menu_title: String::new(),
            menu_items: Vec::new(),
            menu_actions: Vec::new(),
            menu_selected: 0,
            external_command: None,
            scroll_past_bottom: false,
            screen_mode: None,
            ignore_patterns: Vec::new(),
            theme_border: ratatui::style::Color::Blue,
            theme_selection: ratatui::style::Color::Yellow,
            theme_status: ratatui::style::Color::Cyan,
            theme_error: ratatui::style::Color::Red,
            focused_panel: crate::ui::panel::PanelId::Projects,
            cached_filtered_indices: HashMap::new(),
        }
    }
}

impl AppState {
    /// Invalidate cached filtered indices for a specific panel.
    fn invalidate_filtered_cache(&mut self, panel: PanelId) {
        self.cached_filtered_indices.remove(&panel);
    }

    /// Invalidate cached filtered indices for all panels.
    fn invalidate_all_filtered_cache(&mut self) {
        self.cached_filtered_indices.clear();
    }

    /// Check whether a panel has an active filter without computing indices.
    fn has_active_filter(&self, panel: PanelId) -> bool {
        let text_filter = self.panel_filters.get(&panel).filter(|f| !f.is_empty());
        let has_hide_stopped = panel == PanelId::Containers && self.hide_stopped_containers;
        text_filter.is_some() || has_hide_stopped
    }

    /// Warm the filtered cache for all panels that have active filters.
    pub(crate) fn warm_filtered_caches(&mut self) {
        for panel in [
            PanelId::Containers,
            PanelId::Images,
            PanelId::Volumes,
            PanelId::Networks,
            PanelId::Projects,
            PanelId::Services,
        ] {
            if self.has_active_filter(panel) && !self.cached_filtered_indices.contains_key(&panel) {
                self.build_filtered_indices(panel);
            }
        }
    }

    pub fn record_tick(&mut self) {
        self.tick_count = self.tick_count.saturating_add(1);
        
        // Clear status message after 30 ticks (approx 7.5s with default 250ms tick)
        if let Some((_, set_at)) = self.status_message
            && self.tick_count > set_at + 30 {
                self.status_message = None;
            }
    }

    pub fn set_status_message(&mut self, message: impl Into<String>) {
        self.status_message = Some((message.into(), self.tick_count));
    }

    pub fn set_error_message(&mut self, message: impl Into<String>) {
        self.error_message = Some(message.into());
    }

    pub fn clear_error_message(&mut self) {
        self.error_message = None;
    }

    pub fn enter_search_mode(&mut self, panel: PanelId) {
        self.input_mode = InputMode::Search;
        if panel == PanelId::Main {
            self.log_filter = Some(String::new());
            self.active_filter_panel = None;
        } else {
            self.active_filter_panel = Some(panel);
            self.panel_filters.entry(panel).or_default();
        }
    }

    pub fn exit_search_mode(&mut self) {
        self.input_mode = InputMode::Normal;
        if let Some(panel) = self.active_filter_panel.take() {
            self.panel_filters.remove(&panel);
            self.invalidate_filtered_cache(panel);
        } else {
            self.log_filter = None;
        }
    }

    pub fn append_search_char(&mut self, ch: char) {
        if let Some(panel) = self.active_filter_panel {
            if let Some(filter) = self.panel_filters.get_mut(&panel) {
                filter.push(ch);
                self.invalidate_filtered_cache(panel);
                self.clamp_selections();
                self.warm_filtered_caches();
            }
        } else if let Some(filter) = &mut self.log_filter {
            filter.push(ch);
        }
    }

    pub fn backspace_search_char(&mut self) {
        if let Some(panel) = self.active_filter_panel {
            if let Some(filter) = self.panel_filters.get_mut(&panel) {
                filter.pop();
                self.invalidate_filtered_cache(panel);
                self.clamp_selections();
                self.warm_filtered_caches();
            }
        } else if let Some(filter) = &mut self.log_filter {
            filter.pop();
        }
    }

    pub fn confirm_search_mode(&mut self) {
        self.input_mode = InputMode::Normal;
        self.active_filter_panel = None;
    }

    pub fn enter_menu_mode(
        &mut self,
        title: impl Into<String>,
        items: Vec<(String, crate::events::DockerAction)>,
    ) {
        self.input_mode = InputMode::Menu;
        self.menu_title = title.into();
        self.menu_items = items.iter().map(|(label, _)| label.clone()).collect();
        self.menu_actions = items.into_iter().map(|(_, action)| action).collect();
        self.menu_selected = 0;
    }

    pub fn exit_menu_mode(&mut self) {
        self.input_mode = InputMode::Normal;
        self.menu_items.clear();
        self.menu_actions.clear();
        self.menu_selected = 0;
    }

    pub fn move_menu_selection(&mut self, delta: isize) {
        if self.menu_items.is_empty() {
            return;
        }
        let count = self.menu_items.len();
        self.menu_selected = if delta >= 0 {
            self.menu_selected
                .saturating_add(delta as usize)
                .min(count - 1)
        } else {
            self.menu_selected.saturating_sub(delta.unsigned_abs())
        };
    }

    pub fn selected_menu_action(&self) -> Option<&crate::events::DockerAction> {
        self.menu_actions.get(self.menu_selected)
    }

    pub fn toggle_hide_stopped(&mut self) {
        self.hide_stopped_containers = !self.hide_stopped_containers;
        self.invalidate_filtered_cache(PanelId::Containers);
        self.clamp_selections();
        self.warm_filtered_caches();
    }

    /// Return the filtered indices for a panel if a filter is active.
    /// This includes both text filters and the hide-stopped toggle for containers.
    pub fn filtered_indices(&self, panel: PanelId) -> Option<&[usize]> {
        self.cached_filtered_indices.get(&panel).map(|v| v.as_slice())
    }

    /// Build and cache filtered indices for a panel. Called on cache miss.
    fn build_filtered_indices(&mut self, panel: PanelId) {
        if self.cached_filtered_indices.contains_key(&panel) {
            return;
        }

        let text_filter = self.panel_filters.get(&panel).filter(|f| !f.is_empty());
        let has_hide_stopped = panel == PanelId::Containers && self.hide_stopped_containers;

        if text_filter.is_none() && !has_hide_stopped {
            return;
        }

        let indices: Vec<usize> = match panel {
            PanelId::Containers => self
                .containers
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    let matches_text = text_filter.is_none_or(|f| container_matches(c, f));
                    let matches_state =
                        !has_hide_stopped || !matches!(c.state.as_deref(), Some("exited") | Some("dead"));
                    matches_text && matches_state
                })
                .map(|(i, _)| i)
                .collect(),
            PanelId::Images => self
                .images
                .iter()
                .enumerate()
                .filter(|(_, img)| text_filter.is_none_or(|f| image_matches(img, f)))
                .map(|(i, _)| i)
                .collect(),
            PanelId::Volumes => self
                .volumes
                .iter()
                .enumerate()
                .filter(|(_, vol)| text_filter.is_none_or(|f| volume_matches(vol, f)))
                .map(|(i, _)| i)
                .collect(),
            PanelId::Networks => self
                .networks
                .iter()
                .enumerate()
                .filter(|(_, net)| text_filter.is_none_or(|f| network_matches(net, f)))
                .map(|(i, _)| i)
                .collect(),
            PanelId::Projects => self
                .projects
                .iter()
                .enumerate()
                .filter(|(_, proj)| text_filter.is_none_or(|f| project_matches(proj, f)))
                .map(|(i, _)| i)
                .collect(),
            PanelId::Services => {
                let services = self.services_for_selected_project();
                services
                    .iter()
                    .enumerate()
                    .filter(|(_, svc)| text_filter.is_none_or(|f| contains_ignore_case(svc, f)))
                    .map(|(i, _)| i)
                    .collect()
            }
            _ => return,
        };

        self.cached_filtered_indices.insert(panel, indices);
    }

    /// Ensure filtered indices are built and return them.
    pub fn ensure_filtered_indices(&mut self, panel: PanelId) -> Option<&[usize]> {
        self.build_filtered_indices(panel);
        self.filtered_indices(panel)
    }

    pub fn get_filtered_count(&self, panel: PanelId) -> usize {
        if !self.has_active_filter(panel) {
            return self.raw_count(panel);
        }
        match self.filtered_indices(panel) {
            Some(indices) => indices.len(),
            None => self.raw_count(panel), // cold cache fallback
        }
    }

    fn raw_count(&self, panel: PanelId) -> usize {
        match panel {
            PanelId::Containers => self.containers.len(),
            PanelId::Projects => self.projects.len(),
            PanelId::Services => self.services_for_selected_project().len(),
            PanelId::Images => self.images.len(),
            PanelId::Volumes => self.volumes.len(),
            PanelId::Networks => self.networks.len(),
            _ => 0,
        }
    }

    /// Adjust the selection for a panel so it points to a valid filtered item.
    /// Returns true if the selection was changed.
    pub fn clamp_selection_to_filtered(&mut self, panel: PanelId) -> bool {
        let indices: Vec<usize> = match self.ensure_filtered_indices(panel) {
            Some(indices) => indices.to_vec(),
            None => return false,
        };
        if indices.is_empty() {
            let changed = self.selected_indexes.get(&panel) != Some(&0);
            self.selected_indexes.insert(panel, 0);
            return changed;
        }
        let current_raw = self.selected_indexes.get(&panel).copied().unwrap_or(0);
        if indices.contains(&current_raw) {
            false
        } else {
            self.selected_indexes.insert(panel, indices[0]);
            true
        }
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
                self.containers = containers.into_iter()
                    .filter(|c| !self.is_ignored(c))
                    .collect();
                self.projects = crate::docker::compose_projects_from_containers(&self.containers);
                self.invalidate_all_filtered_cache();
                self.clamp_selections();
            }
            DockerUpdate::Images(images) => {
                self.images = images;
                self.invalidate_filtered_cache(PanelId::Images);
                self.clamp_selections();
            }
            DockerUpdate::Volumes(volumes) => {
                self.volumes = volumes;
                self.invalidate_filtered_cache(PanelId::Volumes);
                self.clamp_selections();
            }
            DockerUpdate::Networks(networks) => {
                self.networks = networks;
                self.invalidate_filtered_cache(PanelId::Networks);
                self.clamp_selections();
            }
            DockerUpdate::Event(_) => {
                // Events currently trigger container list refreshes in the supervisor,
                // so we don't need to do much here yet, but we could track last event time.
            }
        }
        self.warm_filtered_caches();
    }

    fn is_ignored(&self, container: &crate::docker::ContainerItem) -> bool {
        if self.ignore_patterns.is_empty() {
            return false;
        }
        let name_str = container.names.join(",");
        self.ignore_patterns.iter().any(|pattern| {
            name_str.contains(pattern)
                || container.id.contains(pattern)
                || container.image.contains(pattern)
        })
    }

    pub fn add_log_line(&mut self, line: String, stream: LogStream) {
        if self.log_buffer.len() >= self.log_buffer_capacity {
            self.log_buffer.pop_front();
        }
        self.log_buffer.push_back((line, stream));
        // Enforce total byte cap by dropping oldest lines
        let mut total_bytes: usize = self.log_buffer.iter().map(|(l, _)| l.len()).sum();
        while total_bytes > self.log_buffer_max_bytes && self.log_buffer.len() > 1 {
            if let Some((dropped, _)) = self.log_buffer.pop_front() {
                total_bytes = total_bytes.saturating_sub(dropped.len());
            }
        }
    }

    pub fn clear_log_buffer(&mut self) {
        self.log_buffer.clear();
        self.log_partial_line = None;
        self.main_scroll_offsets.insert(MainContext::Logs, 0);
    }

    pub fn update_stats(&mut self, stats: ContainerStatsSample) {
        self.active_stats = Some(stats.clone());
        if self.stats_history.len() >= self.stats_history_capacity {
            self.stats_history.pop_front();
        }
        self.stats_history.push_back(stats.cpu_percent);

        if self.memory_history.len() >= self.stats_history_capacity {
            self.memory_history.pop_front();
        }
        let mem_pct = if stats.memory_limit > 0 {
            (stats.memory_usage as f64 / stats.memory_limit as f64) * 100.0
        } else {
            0.0
        };
        self.memory_history.push_back(mem_pct);
    }

    pub fn clear_stats(&mut self) {
        self.active_stats = None;
        self.stats_history.clear();
        self.memory_history.clear();
    }

    pub fn set_main_context(&mut self, context: MainContext) {
        if self.active_main_context != context {
            self.active_main_context = context;
            self.exit_help_mode();
            self.clear_log_buffer();
            self.clear_stats();
            self.env_vars.clear();
            self.horizontal_scroll_offsets.clear();
            if context == MainContext::Logs {
                self.logs_follow_bottom = true;
            }
        }
    }

    pub fn scroll_main_up(&mut self) {
        let context = self.active_main_context;
        let current = self.main_scroll_offsets.get(&context).copied().unwrap_or(0);
        self.main_scroll_offsets
            .insert(context, current.saturating_sub(5));
        if context == MainContext::Logs {
            self.logs_follow_bottom = false;
        }
    }

    pub fn scroll_main_down(&mut self) {
        let context = self.active_main_context;
        let current = self.main_scroll_offsets.get(&context).copied().unwrap_or(0);
        self.main_scroll_offsets
            .insert(context, current.saturating_add(5));
        if context == MainContext::Logs {
            self.logs_follow_bottom = false;
        }
    }

    pub fn scroll_main_up_large(&mut self) {
        let context = self.active_main_context;
        let current = self.main_scroll_offsets.get(&context).copied().unwrap_or(0);
        self.main_scroll_offsets
            .insert(context, current.saturating_sub(20));
        if context == MainContext::Logs {
            self.logs_follow_bottom = false;
        }
    }

    pub fn scroll_main_down_large(&mut self) {
        let context = self.active_main_context;
        let current = self.main_scroll_offsets.get(&context).copied().unwrap_or(0);
        self.main_scroll_offsets
            .insert(context, current.saturating_add(20));
        if context == MainContext::Logs {
            self.logs_follow_bottom = false;
        }
    }

    pub fn scroll_main_to_top(&mut self) {
        let context = self.active_main_context;
        self.main_scroll_offsets.insert(context, 0);
        if context == MainContext::Logs {
            self.logs_follow_bottom = false;
        }
    }

    pub fn scroll_main_to_bottom(&mut self) {
        let context = self.active_main_context;
        self.main_scroll_offsets.insert(context, usize::MAX);
        if context == MainContext::Logs {
            self.logs_follow_bottom = true;
        }
    }

    pub fn scroll_main_left(&mut self) {
        let context = self.active_main_context;
        let current = self
            .horizontal_scroll_offsets
            .get(&context)
            .copied()
            .unwrap_or(0);
        self.horizontal_scroll_offsets
            .insert(context, current.saturating_add(10));
    }

    pub fn scroll_main_right(&mut self) {
        let context = self.active_main_context;
        let current = self
            .horizontal_scroll_offsets
            .get(&context)
            .copied()
            .unwrap_or(0);
        self.horizontal_scroll_offsets
            .insert(context, current.saturating_sub(10));
    }

    pub fn reset_horizontal_scroll(&mut self, context: MainContext) {
        self.horizontal_scroll_offsets.remove(&context);
    }

    pub fn enter_confirm_mode(&mut self, action: DockerAction, prompt: impl Into<String>) {
        self.input_mode = InputMode::Confirm;
        self.pending_confirmation = Some((action, prompt.into()));
    }

    pub fn exit_confirm_mode(&mut self) {
        self.input_mode = InputMode::Normal;
        self.pending_confirmation = None;
        self.pending_leader_action = None;
    }

    pub fn enter_help_mode(&mut self) {
        self.input_mode = InputMode::Help;
    }

    pub fn exit_help_mode(&mut self) {
        self.input_mode = InputMode::Normal;
    }

    pub fn request_shutdown(&mut self, reason: ShutdownReason) {
        self.running = false;
        self.last_shutdown_reason = Some(reason);
    }

    pub fn move_selection(&mut self, panel: PanelId, delta: isize) -> bool {
        let indices: Option<Vec<usize>> =
            self.ensure_filtered_indices(panel).map(|s| s.to_vec());
        let count = match &indices {
            Some(indices) => indices.len(),
            None => self.raw_count(panel),
        };

        if count == 0 {
            return false;
        }

        let current_raw = self.selected_indexes.get(&panel).copied().unwrap_or(0);

        let next_raw = if let Some(indices) = &indices {
            let current_filtered_pos = indices
                .iter()
                .position(|&i| i == current_raw)
                .unwrap_or(0);
            let next_filtered_pos = if delta >= 0 {
                current_filtered_pos
                    .saturating_add(delta as usize)
                    .min(indices.len() - 1)
            } else {
                current_filtered_pos.saturating_sub(delta.unsigned_abs())
            };
            indices[next_filtered_pos]
        } else if delta >= 0 {
            current_raw
                .saturating_add(delta as usize)
                .min(count - 1)
        } else {
            current_raw.saturating_sub(delta.unsigned_abs())
        };

        if next_raw != current_raw {
            self.selected_indexes.insert(panel, next_raw);
            // Project selection changed → services list changes
            if panel == PanelId::Projects {
                self.invalidate_filtered_cache(PanelId::Services);
            }
            true
        } else {
            // Ensure we at least have 0 as selection if it wasn't present
            if let std::collections::hash_map::Entry::Vacant(e) = self.selected_indexes.entry(panel)
            {
                let fallback = indices
                    .as_ref()
                    .and_then(|indices| indices.first().copied())
                    .unwrap_or(0);
                e.insert(fallback);
                true
            } else {
                false
            }
        }
    }

    pub fn set_selection(&mut self, panel: PanelId, index: usize) -> bool {
        let indices: Option<Vec<usize>> =
            self.ensure_filtered_indices(panel).map(|s| s.to_vec());
        let count = match &indices {
            Some(indices) => indices.len(),
            None => self.raw_count(panel),
        };
        if count == 0 {
            return false;
        }
        let clamped = index.min(count - 1);
        let current = self.selected_indexes.get(&panel).copied().unwrap_or(0);
        let next_raw = match &indices {
            Some(indices) => indices.get(clamped).copied().unwrap_or(0),
            None => clamped,
        };
        if next_raw != current {
            self.selected_indexes.insert(panel, next_raw);
            // Project selection changed → services list changes
            if panel == PanelId::Projects {
                self.invalidate_filtered_cache(PanelId::Services);
            }
            true
        } else {
            false
        }
    }

    pub fn get_selection(&self, panel: PanelId) -> usize {
        self.selected_indexes.get(&panel).copied().unwrap_or(0)
    }

    /// Get the position of the selected item within the filtered view.
    /// Returns 0 if no filter is active or the item is not in the filtered set.
    pub fn get_filtered_position(&self, panel: PanelId) -> usize {
        let raw_index = self.get_selection(panel);
        if !self.has_active_filter(panel) {
            return raw_index;
        }
        match self.filtered_indices(panel) {
            Some(indices) => indices.iter().position(|&i| i == raw_index).unwrap_or(0),
            None => raw_index, // cold cache fallback
        }
    }

    fn clamp_selections(&mut self) {
        let panels_to_clamp = [
            (PanelId::Containers, self.containers.len()),
            (PanelId::Projects, self.projects.len()),
            (PanelId::Services, self.services_for_selected_project().len()),
            (PanelId::Images, self.images.len()),
            (PanelId::Volumes, self.volumes.len()),
            (PanelId::Networks, self.networks.len()),
        ];

        for (panel, raw_count) in panels_to_clamp {
            if let Some(index) = self.selected_indexes.get_mut(&panel) {
                if raw_count == 0 {
                    *index = 0;
                } else if *index >= raw_count {
                    *index = raw_count - 1;
                }
            }
            // Also ensure selection is valid within filtered view
            self.clamp_selection_to_filtered(panel);
        }
    }

    pub fn selected_container(&self) -> Option<&ContainerItem> {
        let index = self.get_selection(PanelId::Containers);
        self.containers.get(index)
    }

    pub fn selected_project(&self) -> Option<&ComposeProject> {
        let index = self.get_selection(PanelId::Projects);
        self.projects.get(index)
    }

    pub fn selected_image(&self) -> Option<&ImageItem> {
        let index = self.get_selection(PanelId::Images);
        self.images.get(index)
    }

    pub fn selected_volume(&self) -> Option<&VolumeItem> {
        let index = self.get_selection(PanelId::Volumes);
        self.volumes.get(index)
    }

    pub fn selected_network(&self) -> Option<&NetworkItem> {
        let index = self.get_selection(PanelId::Networks);
        self.networks.get(index)
    }

    pub fn services_for_selected_project(&self) -> &[String] {
        self.selected_project()
            .map(|p| p.services.as_slice())
            .unwrap_or(&[])
    }

    pub fn selected_service_name(&self) -> Option<String> {
        let index = self.get_selection(PanelId::Services);
        self.services_for_selected_project().get(index).cloned()
    }

    pub fn container_for_service(&self, project: &str, service: &str) -> Option<&ContainerItem> {
        self.containers.iter().find(|c| {
            c.compose_project.as_deref() == Some(project)
                && c.compose_service.as_deref() == Some(service)
        })
    }
}

fn contains_ignore_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    for (idx, _) in haystack.char_indices() {
        if starts_with_ignore_case(&haystack[idx..], needle) {
            return true;
        }
    }
    false
}

fn starts_with_ignore_case(haystack: &str, needle: &str) -> bool {
    let mut hay_chars = haystack.chars();
    let mut needle_chars = needle.chars();
    loop {
        match (needle_chars.next(), hay_chars.next()) {
            (None, _) => return true,
            (Some(_), None) => return false,
            (Some(n), Some(h)) => {
                if n == h {
                    continue;
                }
                if n.is_ascii() && h.is_ascii() {
                    if !n.eq_ignore_ascii_case(&h) {
                        return false;
                    }
                } else if n.to_lowercase().next() != h.to_lowercase().next() {
                    return false;
                }
            }
        }
    }
}

fn container_matches(container: &ContainerItem, filter: &str) -> bool {
    contains_ignore_case(&container.id, filter)
        || container.names.iter().any(|n| contains_ignore_case(n, filter))
        || contains_ignore_case(&container.image, filter)
        || container
            .state
            .as_deref()
            .is_some_and(|s| contains_ignore_case(s, filter))
        || container
            .status
            .as_deref()
            .is_some_and(|s| contains_ignore_case(s, filter))
}

fn image_matches(image: &ImageItem, filter: &str) -> bool {
    let id_search = image.id.strip_prefix("sha256:").unwrap_or(&image.id);
    contains_ignore_case(id_search, filter)
        || image
            .repo_tags
            .iter()
            .any(|tag| contains_ignore_case(tag, filter))
}

fn volume_matches(volume: &VolumeItem, filter: &str) -> bool {
    contains_ignore_case(&volume.name, filter)
        || contains_ignore_case(&volume.driver, filter)
        || contains_ignore_case(&volume.mountpoint, filter)
}

fn network_matches(network: &NetworkItem, filter: &str) -> bool {
    contains_ignore_case(&network.id, filter)
        || contains_ignore_case(&network.name, filter)
        || contains_ignore_case(&network.driver, filter)
}

fn project_matches(project: &ComposeProject, filter: &str) -> bool {
    contains_ignore_case(&project.name, filter)
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
    fn docker_state_availability() {
        assert!(!DockerState::Unknown.is_available());
        assert!(
            DockerState::Available(DockerInfo {
                server_version: Some("v1".to_string()),
            })
            .is_available()
        );
        assert!(!DockerState::Unavailable("err".to_string()).is_available());
    }

    #[test]
    fn switching_main_context_clears_contextual_state() {
        let mut state = AppState::default();
        state
            .log_buffer
            .push_back(("line".to_string(), LogStream::Stdout));
        state.active_stats = Some(crate::docker::ContainerStatsSample {
            container_id: "c1".to_string(),
            cpu_percent: 1.0,
            memory_usage: 100,
            memory_limit: 1000,
        });
        state.env_vars.push(("KEY".to_string(), "value".to_string()));

        state.set_main_context(MainContext::Stats);
        assert!(state.log_buffer.is_empty());
        assert!(state.active_stats.is_none());
        assert!(state.stats_history.is_empty());
        assert!(state.memory_history.is_empty());
        assert!(state.env_vars.is_empty());
    }

    #[test]
    fn shutdown_flips_running_flag() {
        let mut state = AppState::default();
        state.request_shutdown(ShutdownReason::User);
        assert!(!state.running);
        assert_eq!(state.last_shutdown_reason, Some(ShutdownReason::User));
    }

    #[test]
    fn ignore_patterns_filter_containers() {
        let mut state = AppState {
            ignore_patterns: vec!["temp-".to_string()],
            ..AppState::default()
        };
        let containers = vec![
            crate::docker::ContainerItem {
                id: "a".to_string(),
                names: vec!["web".to_string()],
                image: "nginx".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            crate::docker::ContainerItem {
                id: "b".to_string(),
                names: vec!["temp-worker".to_string()],
                image: "busybox".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
        },
        ];
        state.apply_docker_update(crate::docker::DockerUpdate::Containers(containers));
        assert_eq!(state.containers.len(), 1);
        assert_eq!(state.containers[0].names[0], "web");
    }

    #[test]
    fn clears_status_message_after_ticks() {
        let mut state = AppState::default();
        state.set_status_message("hello");
        assert!(state.status_message.is_some());

        for _ in 0..30 {
            state.record_tick();
        }
        assert!(state.status_message.is_some());

        state.record_tick();
        assert!(state.status_message.is_none());
    }

    #[test]
    fn log_buffer_respects_capacity() {
        let mut state = AppState {
            log_buffer_capacity: 3,
            ..AppState::default()
        };

        state.add_log_line("a".to_string(), LogStream::Stdout);
        state.add_log_line("b".to_string(), LogStream::Stderr);
        state.add_log_line("c".to_string(), LogStream::Stdout);
        assert_eq!(state.log_buffer.len(), 3);
        assert_eq!(
            state.log_buffer.front(),
            Some(&("a".to_string(), LogStream::Stdout))
        );

        state.add_log_line("d".to_string(), LogStream::Console);
        assert_eq!(state.log_buffer.len(), 3);
        assert_eq!(
            state.log_buffer.front(),
            Some(&("b".to_string(), LogStream::Stderr))
        );
        assert_eq!(
            state.log_buffer.back(),
            Some(&("d".to_string(), LogStream::Console))
        );
    }

    #[test]
    fn log_buffer_respects_byte_cap() {
        let mut state = AppState {
            log_buffer_capacity: 100,
            log_buffer_max_bytes: 20,
            ..AppState::default()
        };

        // Each line is 8 bytes, so 3 lines = 24 bytes > 20 byte cap
        state.add_log_line("12345678".to_string(), LogStream::Stdout);
        state.add_log_line("12345678".to_string(), LogStream::Stdout);
        state.add_log_line("12345678".to_string(), LogStream::Stdout);

        // Should have dropped oldest lines to stay under 20 bytes
        assert!(state.log_buffer.len() < 3);
        let total_bytes: usize = state.log_buffer.iter().map(|(l, _)| l.len()).sum();
        assert!(total_bytes <= 20, "total bytes {total_bytes} exceeded cap");
    }

    #[test]
    fn log_buffer_keeps_at_least_one_line_under_byte_cap() {
        let mut state = AppState {
            log_buffer_capacity: 100,
            log_buffer_max_bytes: 5,
            ..AppState::default()
        };

        // A single 10-byte line exceeds the cap, but should still be kept
        state.add_log_line("1234567890".to_string(), LogStream::Stdout);
        assert_eq!(state.log_buffer.len(), 1);
        assert_eq!(state.log_buffer.front().map(|(l, _)| l.as_str()), Some("1234567890"));
    }

    #[test]
    fn error_message_can_be_set_and_cleared() {
        let mut state = AppState::default();
        assert!(state.error_message.is_none());

        state.set_error_message("something went wrong");
        assert_eq!(state.error_message, Some("something went wrong".to_string()));

        state.clear_error_message();
        assert!(state.error_message.is_none());
    }

    #[test]
    fn search_mode_lifecycle() {
        let mut state = AppState::default();
        assert_eq!(state.input_mode, InputMode::Normal);
        assert!(state.log_filter.is_none());

        state.enter_search_mode(PanelId::Main);
        assert_eq!(state.input_mode, InputMode::Search);
        assert_eq!(state.log_filter, Some(String::new()));

        state.append_search_char('e');
        state.append_search_char('r');
        assert_eq!(state.log_filter, Some("er".to_string()));

        state.backspace_search_char();
        assert_eq!(state.log_filter, Some("e".to_string()));

        state.confirm_search_mode();
        assert_eq!(state.input_mode, InputMode::Normal);
        assert_eq!(state.log_filter, Some("e".to_string()));

        state.exit_search_mode();
        assert_eq!(state.input_mode, InputMode::Normal);
        assert!(state.log_filter.is_none());
    }

    #[test]
    fn panel_filter_lifecycle() {
        let mut state = AppState::default();
        state.containers = vec![
            ContainerItem {
                id: "c1".to_string(),
                names: vec!["web".to_string()],
                image: "nginx".to_string(),
                state: Some("running".to_string()),
                status: Some("Up".to_string()),
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "c2".to_string(),
                names: vec!["db".to_string()],
                image: "postgres".to_string(),
                state: Some("running".to_string()),
                status: Some("Up".to_string()),
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
        },
        ];
        state.selected_indexes.insert(PanelId::Containers, 1); // select "db"

        state.enter_search_mode(PanelId::Containers);
        assert_eq!(state.input_mode, InputMode::Search);
        assert_eq!(state.active_filter_panel, Some(PanelId::Containers));
        assert_eq!(state.panel_filters.get(&PanelId::Containers), Some(&String::new()));

        // Filter to "web" — "db" should no longer be selected
        state.append_search_char('w');
        state.append_search_char('e');
        state.append_search_char('b');
        assert_eq!(
            state.panel_filters.get(&PanelId::Containers),
            Some(&"web".to_string())
        );
        // Selection should have moved to "web" (index 0)
        assert_eq!(state.get_selection(PanelId::Containers), 0);

        // Confirm keeps filter
        state.confirm_search_mode();
        assert_eq!(state.input_mode, InputMode::Normal);
        assert_eq!(
            state.panel_filters.get(&PanelId::Containers),
            Some(&"web".to_string())
        );

        // Exit clears filter
        state.enter_search_mode(PanelId::Containers);
        state.exit_search_mode();
        assert!(state.panel_filters.get(&PanelId::Containers).is_none());
        assert_eq!(state.input_mode, InputMode::Normal);
    }

    #[test]
    fn filtered_indices_returns_matching_items() {
        let mut state = AppState::default();
        state.containers = vec![
            ContainerItem {
                id: "c1".to_string(),
                names: vec!["web-server".to_string()],
                image: "nginx".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "c2".to_string(),
                names: vec!["db-server".to_string()],
                image: "postgres".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
        },
        ];
        state.panel_filters.insert(PanelId::Containers, "web".to_string());

        let indices = state.ensure_filtered_indices(PanelId::Containers).unwrap();
        assert_eq!(indices, vec![0]);

        // Case-insensitive
        state.panel_filters.insert(PanelId::Containers, "POSTGRES".to_string());
        state.invalidate_filtered_cache(PanelId::Containers);
        let indices = state.ensure_filtered_indices(PanelId::Containers).unwrap();
        assert_eq!(indices, vec![1]);
    }

    #[test]
    fn move_selection_respects_panel_filter() {
        let mut state = AppState::default();
        state.containers = vec![
            ContainerItem {
                id: "c1".to_string(),
                names: vec!["alpha".to_string()],
                image: "img".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "c2".to_string(),
                names: vec!["beta".to_string()],
                image: "img".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "c3".to_string(),
                names: vec!["gamma".to_string()],
                image: "img".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
        },
        ];
        state.panel_filters.insert(PanelId::Containers, "alp".to_string());
        // "alpha" matches; selected at alpha (0)
        state.selected_indexes.insert(PanelId::Containers, 0);

        // Only alpha matches, so move down should stay at alpha
        assert!(!state.move_selection(PanelId::Containers, 1));
        assert_eq!(state.get_selection(PanelId::Containers), 0);

        // Move down again should still stay at alpha
        assert!(!state.move_selection(PanelId::Containers, 1));
        assert_eq!(state.get_selection(PanelId::Containers), 0);
    }

    #[test]
    fn hide_stopped_filters_exited_and_dead_containers() {
        let mut state = AppState::default();
        state.containers = vec![
            ContainerItem {
                id: "c1".to_string(),
                names: vec!["web".to_string()],
                image: "img".to_string(),
                state: Some("running".to_string()),
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "c2".to_string(),
                names: vec!["db".to_string()],
                image: "img".to_string(),
                state: Some("exited".to_string()),
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "c3".to_string(),
                names: vec!["cache".to_string()],
                image: "img".to_string(),
                state: Some("dead".to_string()),
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
        },
        ];
        state.selected_indexes.insert(PanelId::Containers, 2);

        // Without hide stopped, all containers are visible
        assert!(state.filtered_indices(PanelId::Containers).is_none());
        assert_eq!(state.get_filtered_count(PanelId::Containers), 3);

        // Enable hide stopped
        state.toggle_hide_stopped();
        assert!(state.hide_stopped_containers);

        // Only running container should be visible
        let indices = state.filtered_indices(PanelId::Containers).unwrap();
        assert_eq!(indices, vec![0]);
        assert_eq!(state.get_filtered_count(PanelId::Containers), 1);

        // Selection should have been clamped to the first visible item
        assert_eq!(state.get_selection(PanelId::Containers), 0);

        // Disable hide stopped
        state.toggle_hide_stopped();
        assert!(!state.hide_stopped_containers);
        assert_eq!(state.get_filtered_count(PanelId::Containers), 3);
    }

    #[test]
    fn hide_stopped_works_with_text_filter() {
        let mut state = AppState::default();
        state.containers = vec![
            ContainerItem {
                id: "c1".to_string(),
                names: vec!["web-server".to_string()],
                image: "img".to_string(),
                state: Some("running".to_string()),
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "c2".to_string(),
                names: vec!["web-old".to_string()],
                image: "img".to_string(),
                state: Some("exited".to_string()),
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            ContainerItem {
                id: "c3".to_string(),
                names: vec!["db".to_string()],
                image: "img".to_string(),
                state: Some("running".to_string()),
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
        },
        ];
        state.panel_filters.insert(PanelId::Containers, "web".to_string());
        state.hide_stopped_containers = true;

        // Text filter matches web-server and web-old
        // Hide stopped filters out web-old (exited)
        // Result: only web-server (running)
        let indices = state.ensure_filtered_indices(PanelId::Containers).unwrap();
        assert_eq!(indices, vec![0]);
    }

    #[test]
    fn stats_history_tracks_cpu_and_respects_capacity() {
        let mut state = AppState {
            stats_history_capacity: 3,
            ..AppState::default()
        };

        state.update_stats(ContainerStatsSample {
            container_id: "c1".to_string(),
            cpu_percent: 10.0,
            memory_usage: 100,
            memory_limit: 1000,
        });
        assert_eq!(state.stats_history.len(), 1);
        assert_eq!(state.stats_history.back(), Some(&10.0));
        assert_eq!(state.memory_history.len(), 1);
        assert_eq!(state.memory_history.back(), Some(&10.0)); // 100/1000 * 100

        state.update_stats(ContainerStatsSample {
            container_id: "c1".to_string(),
            cpu_percent: 20.0,
            memory_usage: 200,
            memory_limit: 1000,
        });
        state.update_stats(ContainerStatsSample {
            container_id: "c1".to_string(),
            cpu_percent: 30.0,
            memory_usage: 300,
            memory_limit: 1000,
        });
        assert_eq!(state.stats_history.len(), 3);
        assert_eq!(state.memory_history.len(), 3);

        state.update_stats(ContainerStatsSample {
            container_id: "c1".to_string(),
            cpu_percent: 40.0,
            memory_usage: 400,
            memory_limit: 1000,
        });
        assert_eq!(state.stats_history.len(), 3);
        assert_eq!(state.stats_history.back(), Some(&40.0));
        assert_eq!(state.stats_history.front(), Some(&20.0));
        assert_eq!(state.memory_history.len(), 3);
        assert_eq!(state.memory_history.back(), Some(&40.0)); // 400/1000 * 100
        assert_eq!(state.memory_history.front(), Some(&20.0)); // 200/1000 * 100
    }

    #[test]
    fn logs_follow_bottom_by_default() {
        let state = AppState::default();
        assert!(state.logs_follow_bottom);
    }

    #[test]
    fn scroll_main_up_disables_follow_bottom() {
        let mut state = AppState {
            active_main_context: MainContext::Logs,
            ..AppState::default()
        };
        state.scroll_main_up();
        assert!(!state.logs_follow_bottom);
        assert_eq!(state.main_scroll_offsets.get(&MainContext::Logs), Some(&0));
    }

    #[test]
    fn scroll_main_down_increases_offset() {
        let mut state = AppState {
            active_main_context: MainContext::Config,
            ..AppState::default()
        };
        state.scroll_main_down();
        assert_eq!(state.main_scroll_offsets.get(&MainContext::Config), Some(&5));
    }

    #[test]
    fn scroll_main_up_large_scrolls_more() {
        let mut state = AppState {
            active_main_context: MainContext::Config,
            ..AppState::default()
        };
        state.scroll_main_down_large();
        assert_eq!(state.main_scroll_offsets.get(&MainContext::Config), Some(&20));
    }

    #[test]
    fn scroll_main_up_large_disables_follow_bottom() {
        let mut state = AppState {
            active_main_context: MainContext::Logs,
            ..AppState::default()
        };
        state.scroll_main_up_large();
        assert!(!state.logs_follow_bottom);
        assert_eq!(state.main_scroll_offsets.get(&MainContext::Logs), Some(&0));
    }

    #[test]
    fn scroll_main_to_bottom_re_enables_follow() {
        let mut state = AppState {
            active_main_context: MainContext::Logs,
            ..AppState::default()
        };
        state.scroll_main_up();
        assert!(!state.logs_follow_bottom);
        state.scroll_main_to_bottom();
        assert!(state.logs_follow_bottom);
    }

    #[test]
    fn set_main_context_to_logs_resets_follow_bottom() {
        let mut state = AppState {
            active_main_context: MainContext::Stats,
            logs_follow_bottom: false,
            ..AppState::default()
        };
        state.set_main_context(MainContext::Logs);
        assert!(state.logs_follow_bottom);
    }

    #[test]
    fn clear_log_buffer_resets_logs_scroll() {
        let mut state = AppState::default();
        state.main_scroll_offsets.insert(MainContext::Logs, 42);
        state.clear_log_buffer();
        assert_eq!(state.main_scroll_offsets.get(&MainContext::Logs), Some(&0));
    }

    #[test]
    fn horizontal_scroll_offsets_increase_and_decrease() {
        let mut state = AppState {
            active_main_context: MainContext::Config,
            ..AppState::default()
        };

        state.scroll_main_left();
        assert_eq!(
            state.horizontal_scroll_offsets.get(&MainContext::Config),
            Some(&10)
        );

        state.scroll_main_left();
        assert_eq!(
            state.horizontal_scroll_offsets.get(&MainContext::Config),
            Some(&20)
        );

        state.scroll_main_right();
        assert_eq!(
            state.horizontal_scroll_offsets.get(&MainContext::Config),
            Some(&10)
        );

        state.scroll_main_right();
        state.scroll_main_right();
        assert_eq!(
            state.horizontal_scroll_offsets.get(&MainContext::Config),
            Some(&0)
        );
    }

    #[test]
    fn switching_main_context_resets_horizontal_scroll() {
        let mut state = AppState {
            active_main_context: MainContext::Config,
            ..AppState::default()
        };
        state.scroll_main_left();
        assert!(state.horizontal_scroll_offsets.contains_key(&MainContext::Config));

        state.set_main_context(MainContext::Logs);
        assert!(!state.horizontal_scroll_offsets.contains_key(&MainContext::Config));
    }

    #[test]
    fn menu_mode_lifecycle() {
        let mut state = AppState::default();
        assert_eq!(state.input_mode, InputMode::Normal);

        state.enter_menu_mode(
            "Test Menu",
            vec![
                ("Option 1".to_string(), crate::events::DockerAction::PruneImages),
                ("Option 2".to_string(), crate::events::DockerAction::PruneVolumes),
            ],
        );
        assert_eq!(state.input_mode, InputMode::Menu);
        assert_eq!(state.menu_title, "Test Menu");
        assert_eq!(state.menu_items, vec!["Option 1", "Option 2"]);
        assert_eq!(state.menu_selected, 0);

        state.move_menu_selection(1);
        assert_eq!(state.menu_selected, 1);

        state.move_menu_selection(-1);
        assert_eq!(state.menu_selected, 0);

        state.move_menu_selection(10);
        assert_eq!(state.menu_selected, 1); // clamped to last item

        assert_eq!(
            state.selected_menu_action(),
            Some(&crate::events::DockerAction::PruneVolumes)
        );

        state.exit_menu_mode();
        assert_eq!(state.input_mode, InputMode::Normal);
        assert!(state.menu_items.is_empty());
    }
}
