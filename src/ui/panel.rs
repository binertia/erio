use std::{borrow::Cow, fmt::Write};

use ratatui::{
    Frame,
    layout::{Constraint, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState, Wrap},
};

use crate::{
    docker::LogStream,
    state::{AppState, DockerState, InputMode, MainContext},
};

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

/// The content to render inside a panel. `Text` is rendered as a `Paragraph`;
/// `Table` is rendered as a `ratatui::widgets::Table` with selection highlighting.
pub enum PanelContent<'a> {
    Text(std::borrow::Cow<'static, str>),
    StyledText(Text<'a>),
    Table {
        header: Option<Vec<&'static str>>,
        rows: Vec<Vec<Cow<'a, str>>>,
        row_styles: Vec<Style>,
        selected: usize,
        offset: usize,
        widths: Vec<Constraint>,
    },
}

#[derive(Debug, Clone)]
pub struct RenderContext<'a> {
    pub state: &'a AppState,
    pub focused: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelSpec {
    id: PanelId,
    focusable: bool,
    title: &'static str,
    title_buffer: std::cell::RefCell<String>,
}

impl PanelSpec {
    pub fn new(id: PanelId, focusable: bool, title: &'static str) -> Self {
        Self {
            id,
            focusable,
            title,
            title_buffer: std::cell::RefCell::new(String::with_capacity(32)),
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
                .fg(context.state.theme_border)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };

        let filter = context
            .state
            .panel_filters
            .get(&self.id)
            .map(|s| s.as_str());

        let content = match self.id {
            PanelId::Projects => render_project_list(
                &context.state.projects,
                context.state.get_selection(PanelId::Projects),
                filter,
                area.width,
                area.height.saturating_sub(2),
            ),
            PanelId::Services => render_service_list(
                context.state.services_for_selected_project(),
                context.state.get_selection(PanelId::Services),
                filter,
                area.width,
                area.height.saturating_sub(2),
            ),
            PanelId::Containers => render_container_list(
                &context.state.containers,
                context.state.get_selection(PanelId::Containers),
                filter,
                area.width,
                area.height.saturating_sub(2),
            ),
            PanelId::Images => render_image_list(
                &context.state.images,
                context.state.get_selection(PanelId::Images),
                filter,
                area.width,
                area.height.saturating_sub(2),
            ),
            PanelId::Volumes => render_volume_list(
                &context.state.volumes,
                context.state.get_selection(PanelId::Volumes),
                filter,
                area.width,
                area.height.saturating_sub(2),
            ),
            PanelId::Networks => render_network_list(
                &context.state.networks,
                context.state.get_selection(PanelId::Networks),
                filter,
                area.width,
                area.height.saturating_sub(2),
            ),
            PanelId::Main => render_main_panel(
                context.state,
                area.height.saturating_sub(2) as usize,
                area.width.saturating_sub(2) as usize,
            ),
            PanelId::Status => PanelContent::Text(std::borrow::Cow::Owned(status_content(
                context.state,
                area.width.saturating_sub(2) as usize,
            ))),
        };

        // Show "Loading..." for empty list panels while Docker is still connecting.
        let content = if matches!(context.state.docker, DockerState::Unknown) {
            match content {
                PanelContent::Text(ref msg)
                    if msg == "No projects found"
                        || msg == "No services found"
                        || msg == "No containers found"
                        || msg == "No images found"
                        || msg == "No volumes found"
                        || msg == "No networks found" =>
                {
                    PanelContent::Text(std::borrow::Cow::Borrowed("Loading..."))
                }
                _ => content,
            }
        } else {
            content
        };

        let title = self.dynamic_title(&context);
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(border_style);

        match content {
            PanelContent::Text(text) => {
                frame.render_widget(
                    Paragraph::new(text).block(block).wrap(Wrap::default()),
                    area,
                );
            }
            PanelContent::StyledText(text) => {
                frame.render_widget(
                    Paragraph::new(text).block(block).wrap(Wrap::default()),
                    area,
                );
            }
            PanelContent::Table {
                header,
                rows,
                row_styles,
                selected,
                offset,
                widths,
            } => {
                let table_rows = rows.into_iter().zip(row_styles).map(|(row, style)| {
                    Row::new(row.into_iter().map(Cell::from)).style(style)
                });
                let mut table = Table::new(table_rows, widths)
                    .block(block)
                    .row_highlight_style(
                        Style::default()
                            .fg(context.state.theme_selection)
                            .add_modifier(Modifier::BOLD),
                    )
                    .highlight_symbol("");
                if let Some(header) = header {
                    let header_row = Row::new(header.iter().map(|h| Cell::from(*h)))
                        .style(
                            Style::default()
                                .fg(context.state.theme_status)
                                .add_modifier(Modifier::BOLD),
                        );
                    table = table.header(header_row);
                }
                let mut table_state = TableState::default()
                    .with_selected(Some(selected))
                    .with_offset(offset);
                frame.render_stateful_widget(table, area, &mut table_state);
            }
        }
    }

    fn dynamic_title(&self, context: &RenderContext<'_>) -> Line<'static> {
        let (raw_count, filtered_count) = match self.id {
            PanelId::Containers => (
                context.state.containers.len(),
                context.state.get_filtered_count(PanelId::Containers),
            ),
            PanelId::Projects => (
                context.state.projects.len(),
                context.state.get_filtered_count(PanelId::Projects),
            ),
            PanelId::Services => {
                let raw = context.state.services_for_selected_project().len();
                (raw, context.state.get_filtered_count(PanelId::Services))
            }
            PanelId::Images => (
                context.state.images.len(),
                context.state.get_filtered_count(PanelId::Images),
            ),
            PanelId::Volumes => (
                context.state.volumes.len(),
                context.state.get_filtered_count(PanelId::Volumes),
            ),
            PanelId::Networks => (
                context.state.networks.len(),
                context.state.get_filtered_count(PanelId::Networks),
            ),
            PanelId::Main => return self.main_panel_title(context),
            _ => return Line::from(self.title),
        };

        if raw_count == 0 {
            return Line::from(self.title);
        }

        let selected = context.state.get_filtered_position(self.id) + 1;
        let mut buf = self.title_buffer.borrow_mut();
        buf.clear();
        if filtered_count != raw_count {
            let _ = write!(buf, "{} ({}/{} of {})", self.title, selected, filtered_count, raw_count);
        } else {
            let _ = write!(buf, "{} ({}/{})", self.title, selected, filtered_count);
        }
        Line::from(std::mem::take(&mut *buf))
    }

    fn main_panel_title(&self, context: &RenderContext<'_>) -> Line<'static> {
        if context.state.input_mode == InputMode::Leader {
            return Line::from("Leader");
        }
        if context.state.input_mode == InputMode::Confirm {
            return Line::from("Confirm");
        }
        if context.state.input_mode == InputMode::Help {
            return Line::from("Help");
        }
        if context.state.input_mode == InputMode::Menu {
            return Line::from(context.state.menu_title.clone());
        }
        let mut buf = self.title_buffer.borrow_mut();
        buf.clear();
        match context.state.active_main_context {
            MainContext::Logs => {
                let name = context
                    .state
                    .selected_container()
                    .and_then(|c| c.names.first())
                    .map(|s| s.as_str())
                    .unwrap_or("?");
                let _ = write!(buf, "Logs: {name}");
            }
            MainContext::Stats => {
                let name = context
                    .state
                    .selected_container()
                    .and_then(|c| c.names.first())
                    .map(|s| s.as_str())
                    .unwrap_or("?");
                if let Some(stats) = &context.state.active_stats {
                    let _ = write!(buf, "Stats: {name} ({:.1}% CPU)", stats.cpu_percent);
                } else {
                    let _ = write!(buf, "Stats: {name}");
                }
            }
            MainContext::Config => {
                let name = context
                    .state
                    .selected_container()
                    .and_then(|c| c.names.first())
                    .map(|s| s.as_str())
                    .unwrap_or("?");
                let _ = write!(buf, "Config: {name}");
            }
            MainContext::Env => {
                let name = context
                    .state
                    .selected_container()
                    .and_then(|c| c.names.first())
                    .map(|s| s.as_str())
                    .unwrap_or("?");
                let _ = write!(buf, "Env: {name}");
            }
            MainContext::ImageInfo => {
                let name = context
                    .state
                    .selected_image()
                    .and_then(|i| i.repo_tags.first())
                    .map(|s| s.as_str())
                    .unwrap_or("?");
                let _ = write!(buf, "Image: {name}");
            }
            MainContext::VolumeInfo => {
                let name = context
                    .state
                    .selected_volume()
                    .map(|v| v.name.as_str())
                    .unwrap_or("?");
                let _ = write!(buf, "Volume: {name}");
            }
            MainContext::NetworkInfo => {
                let name = context
                    .state
                    .selected_network()
                    .map(|n| n.name.as_str())
                    .unwrap_or("?");
                let _ = write!(buf, "Network: {name}");
            }
        }
        Line::from(std::mem::take(&mut *buf))
    }
}

fn render_network_list<'a>(
    networks: &'a [crate::docker::NetworkItem],
    selected_index: usize,
    filter: Option<&'a str>,
    _max_width: u16,
    max_lines: u16,
) -> PanelContent<'a> {
    let is_filtered = filter.is_some_and(|f| !f.is_empty());
    let filtered_indices: Vec<usize> = if is_filtered {
        networks
            .iter()
            .enumerate()
            .filter(|(_, net)| network_matches(net, filter.unwrap()))
            .map(|(i, _)| i)
            .collect()
    } else {
        Vec::new()
    };

    if networks.is_empty() {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No networks found"));
    }
    let total = if is_filtered { filtered_indices.len() } else { networks.len() };
    if total == 0 {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No networks match filter"));
    }

    let filtered_selected = if is_filtered {
        filtered_indices
            .iter()
            .position(|&i| i == selected_index)
            .unwrap_or(0)
    } else {
        selected_index.min(total.saturating_sub(1))
    };

    let header_lines = if max_lines > 2 { 1 } else { 0 };
    let visible_count = max_lines.saturating_sub(header_lines).max(1) as usize;
    let scroll = list_scroll_offset(filtered_selected, total, visible_count);
    let end = (scroll + visible_count).min(total);

    let mut rows = Vec::with_capacity(end.saturating_sub(scroll));
    let mut add_row = |idx: usize| {
        let network = &networks[idx];
        rows.push(vec![
            truncate_width(&network.id, 12),
            truncate_width(&network.name, 28),
            truncate_width(&network.driver, 13),
        ]);
    };
    if is_filtered {
        for &idx in &filtered_indices[scroll..end] {
            add_row(idx);
        }
    } else {
        for idx in scroll..end {
            add_row(idx);
        }
    }

    let row_count = rows.len();
    PanelContent::Table {
        header: if header_lines > 0 {
            Some(vec!["ID", "NAME", "DRIVER"])
        } else {
            None
        },
        rows,
        row_styles: vec![Style::default(); row_count],
        selected: filtered_selected.saturating_sub(scroll),
        offset: 0,
        widths: vec![
            Constraint::Length(12),
            Constraint::Min(10),
            Constraint::Length(15),
        ],
    }
}

fn render_volume_list<'a>(
    volumes: &'a [crate::docker::VolumeItem],
    selected_index: usize,
    filter: Option<&'a str>,
    _max_width: u16,
    max_lines: u16,
) -> PanelContent<'a> {
    let is_filtered = filter.is_some_and(|f| !f.is_empty());
    let filtered_indices: Vec<usize> = if is_filtered {
        volumes
            .iter()
            .enumerate()
            .filter(|(_, vol)| volume_matches(vol, filter.unwrap()))
            .map(|(i, _)| i)
            .collect()
    } else {
        Vec::new()
    };

    if volumes.is_empty() {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No volumes found"));
    }
    let total = if is_filtered { filtered_indices.len() } else { volumes.len() };
    if total == 0 {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No volumes match filter"));
    }

    let filtered_selected = if is_filtered {
        filtered_indices
            .iter()
            .position(|&i| i == selected_index)
            .unwrap_or(0)
    } else {
        selected_index.min(total.saturating_sub(1))
    };

    let header_lines = if max_lines > 2 { 1 } else { 0 };
    let visible_count = max_lines.saturating_sub(header_lines).max(1) as usize;
    let scroll = list_scroll_offset(filtered_selected, total, visible_count);
    let end = (scroll + visible_count).min(total);

    let mut rows = Vec::with_capacity(end.saturating_sub(scroll));
    let mut add_row = |idx: usize| {
        let volume = &volumes[idx];
        rows.push(vec![
            truncate_width(&volume.name, 28),
            truncate_width(&volume.driver, 13),
            truncate_width(&volume.mountpoint, 28),
        ]);
    };
    if is_filtered {
        for &idx in &filtered_indices[scroll..end] {
            add_row(idx);
        }
    } else {
        for idx in scroll..end {
            add_row(idx);
        }
    }

    let row_count = rows.len();
    PanelContent::Table {
        header: if header_lines > 0 {
            Some(vec!["NAME", "DRIVER", "MOUNTPOINT"])
        } else {
            None
        },
        rows,
        row_styles: vec![Style::default(); row_count],
        selected: filtered_selected.saturating_sub(scroll),
        offset: 0,
        widths: vec![
            Constraint::Min(15),
            Constraint::Length(15),
            Constraint::Min(15),
        ],
    }
}

fn render_image_list<'a>(
    images: &'a [crate::docker::ImageItem],
    selected_index: usize,
    filter: Option<&'a str>,
    _max_width: u16,
    max_lines: u16,
) -> PanelContent<'a> {
    let is_filtered = filter.is_some_and(|f| !f.is_empty());
    let filtered_indices: Vec<usize> = if is_filtered {
        images
            .iter()
            .enumerate()
            .filter(|(_, img)| image_matches(img, filter.unwrap()))
            .map(|(i, _)| i)
            .collect()
    } else {
        Vec::new()
    };

    if images.is_empty() {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No images found"));
    }
    let total = if is_filtered { filtered_indices.len() } else { images.len() };
    if total == 0 {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No images match filter"));
    }

    let filtered_selected = if is_filtered {
        filtered_indices
            .iter()
            .position(|&i| i == selected_index)
            .unwrap_or(0)
    } else {
        selected_index.min(total.saturating_sub(1))
    };

    let header_lines = if max_lines > 2 { 1 } else { 0 };
    let visible_count = max_lines.saturating_sub(header_lines).max(1) as usize;
    let scroll = list_scroll_offset(filtered_selected, total, visible_count);
    let end = (scroll + visible_count).min(total);

    let mut rows = Vec::with_capacity(end.saturating_sub(scroll));
    let mut add_row = |idx: usize| {
        let image = &images[idx];
        let id = truncate_width(
            image.id.strip_prefix("sha256:").unwrap_or(&image.id),
            12,
        );

        let (repo, tag) = if let Some(tag_str) = image.repo_tags.first() {
            if tag_str.contains('@') {
                (tag_str.as_str(), "<none>")
            } else if let Some((r, t)) = tag_str.rsplit_once(':') {
                (r, t)
            } else {
                (tag_str.as_str(), "latest")
            }
        } else {
            ("<none>", "<none>")
        };

        rows.push(vec![
            id,
            truncate_width(repo, 28),
            truncate_width(tag, 13),
            Cow::Owned(format_bytes(image.size as u64)),
        ]);
    };
    if is_filtered {
        for &idx in &filtered_indices[scroll..end] {
            add_row(idx);
        }
    } else {
        for idx in scroll..end {
            add_row(idx);
        }
    }

    let row_count = rows.len();
    PanelContent::Table {
        header: if header_lines > 0 {
            Some(vec!["ID", "REPOSITORY", "TAG", "SIZE"])
        } else {
            None
        },
        rows,
        row_styles: vec![Style::default(); row_count],
        selected: filtered_selected.saturating_sub(scroll),
        offset: 0,
        widths: vec![
            Constraint::Length(12),
            Constraint::Min(10),
            Constraint::Length(15),
            Constraint::Length(10),
        ],
    }
}

fn render_project_list<'a>(
    projects: &'a [crate::docker::ComposeProject],
    selected_index: usize,
    filter: Option<&'a str>,
    max_width: u16,
    max_lines: u16,
) -> PanelContent<'a> {
    let is_filtered = filter.is_some_and(|f| !f.is_empty());
    let filtered_indices: Vec<usize> = if is_filtered {
        projects
            .iter()
            .enumerate()
            .filter(|(_, proj)| project_matches(proj, filter.unwrap()))
            .map(|(i, _)| i)
            .collect()
    } else {
        Vec::new()
    };

    if projects.is_empty() {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No projects found"));
    }
    let total = if is_filtered { filtered_indices.len() } else { projects.len() };
    if total == 0 {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No projects match filter"));
    }

    let filtered_selected = if is_filtered {
        filtered_indices
            .iter()
            .position(|&i| i == selected_index)
            .unwrap_or(0)
    } else {
        selected_index.min(total.saturating_sub(1))
    };

    let content_width = max_width.saturating_sub(2) as usize;
    let visible_count = max_lines.max(1) as usize;
    let scroll = list_scroll_offset(filtered_selected, total, visible_count);
    let end = (scroll + visible_count).min(total);

    let mut output = String::with_capacity(visible_count * 32);
    let mut render_item = |global_i: usize, idx: usize| {
        let prefix = if global_i == filtered_selected { "> " } else { "  " };
        let name = truncate_width(&projects[idx].name, content_width);
        let _ = writeln!(output, "{}{}", prefix, name);
    };
    if is_filtered {
        for (global_i, &idx) in filtered_indices.iter().enumerate().skip(scroll).take(end - scroll) {
            render_item(global_i, idx);
        }
    } else {
        for global_i in scroll..end {
            render_item(global_i, global_i);
        }
    }
    PanelContent::Text(std::borrow::Cow::Owned(output))
}

fn render_service_list<'a>(
    services: &'a [String],
    selected_index: usize,
    filter: Option<&'a str>,
    max_width: u16,
    max_lines: u16,
) -> PanelContent<'a> {
    let is_filtered = filter.is_some_and(|f| !f.is_empty());
    let filtered_indices: Vec<usize> = if is_filtered {
        services
            .iter()
            .enumerate()
            .filter(|(_, svc)| contains_ignore_case(svc, filter.unwrap()))
            .map(|(i, _)| i)
            .collect()
    } else {
        Vec::new()
    };

    if services.is_empty() {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No services found"));
    }
    let total = if is_filtered { filtered_indices.len() } else { services.len() };
    if total == 0 {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No services match filter"));
    }

    let filtered_selected = if is_filtered {
        filtered_indices
            .iter()
            .position(|&i| i == selected_index)
            .unwrap_or(0)
    } else {
        selected_index.min(total.saturating_sub(1))
    };

    let content_width = max_width.saturating_sub(2) as usize;
    let visible_count = max_lines.max(1) as usize;
    let scroll = list_scroll_offset(filtered_selected, total, visible_count);
    let end = (scroll + visible_count).min(total);

    let mut output = String::with_capacity(visible_count * 32);
    let mut render_item = |global_i: usize, idx: usize| {
        let prefix = if global_i == filtered_selected { "> " } else { "  " };
        let name = truncate_width(&services[idx], content_width);
        let _ = writeln!(output, "{}{}", prefix, name);
    };
    if is_filtered {
        for (global_i, &idx) in filtered_indices.iter().enumerate().skip(scroll).take(end - scroll) {
            render_item(global_i, idx);
        }
    } else {
        for global_i in scroll..end {
            render_item(global_i, global_i);
        }
    }
    PanelContent::Text(std::borrow::Cow::Owned(output))
}

/// Case-insensitive substring search without allocating temporary strings.
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

fn container_matches(container: &crate::docker::ContainerItem, filter: &str) -> bool {
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

fn image_matches(image: &crate::docker::ImageItem, filter: &str) -> bool {
    let id_search = image.id.strip_prefix("sha256:").unwrap_or(&image.id);
    contains_ignore_case(id_search, filter)
        || image
            .repo_tags
            .iter()
            .any(|tag| contains_ignore_case(tag, filter))
}

fn volume_matches(volume: &crate::docker::VolumeItem, filter: &str) -> bool {
    contains_ignore_case(&volume.name, filter)
        || contains_ignore_case(&volume.driver, filter)
        || contains_ignore_case(&volume.mountpoint, filter)
}

fn network_matches(network: &crate::docker::NetworkItem, filter: &str) -> bool {
    contains_ignore_case(&network.id, filter)
        || contains_ignore_case(&network.name, filter)
        || contains_ignore_case(&network.driver, filter)
}

fn project_matches(project: &crate::docker::ComposeProject, filter: &str) -> bool {
    contains_ignore_case(&project.name, filter)
}

/// Compute the scroll offset for a list so the selected item stays visible.
/// Uses a "centered cursor" strategy: the selected item is kept near the
/// middle of the visible window when possible.
fn list_scroll_offset(selected_index: usize, total: usize, visible_count: usize) -> usize {
    let visible_count = visible_count.min(total).max(1);
    if total <= visible_count {
        return 0;
    }
    let half = visible_count / 2;
    if selected_index < half {
        0
    } else {
        selected_index
            .saturating_sub(half)
            .min(total.saturating_sub(visible_count))
    }
}

fn render_help_text(max_lines: usize, max_width: usize) -> PanelContent<'static> {
    let lines = vec![
        "Keyboard Shortcuts",
        "",
        "Navigation",
        "  j / ↑     Move up",
        "  k / ↓     Move down",
        "  h / l     Previous / Next panel",
        "  Tab       Next panel",
        "  1-6       Focus Projects/Services/Containers/Images/Volumes/Networks",
        "",
        "Main Panel",
        "  l         Logs",
        "  S         Stats",
        "  c         Config",
        "  e         Environment variables (main panel)",
        "  PgUp/PgDn Scroll main panel",
        "  J / K     Scroll main panel (large)",
        "  H / L     Scroll main panel left / right",
        "  Home/End  Jump to top/bottom",
        "",
        "Leader",
        "  Space     Leader prefix (press before any action)",
        "",
        "Actions (press Space first)",
        "  s         Stop container",
        "  u         Start container",
        "  r         Restart container",
        "  E         Exec shell (container/service)",
        "  a         Attach to container/service",
        "  d         Delete / Remove",
        "  p         Pause/unpause (containers) / Prune (others)", 
        "  c         Custom commands (context-aware)",
        "  X         Global custom commands",
        "  U / D     Project up / down (projects panel)",
        "  u / s / r Start / stop / restart (container/service)",
        "  R         Restart policy menu (containers)", 
        "  S         Start service (services panel) / Stats (main)",
        "  b         Bulk operations (per panel)",
        "  e         Toggle hide stopped (containers panel)",
        "  m         View logs (projects/services)", 
        "  w         Open in browser (container/service)",
        "  y / n     Confirm / Cancel",
        "",
        "Other",
        "  /         Search / filter current panel",
        "  Click     Focus panel",
        "  Scroll    Scroll list",
        "  ? / h     Show this help",
        "  Enter     Select menu item",
        "  j / k     Navigate menu",
        "  q         Quit",
    ];
    let mut output = String::with_capacity(lines.len() * max_width);
    for line in lines.into_iter().take(max_lines) {
        let _ = writeln!(output, "{}", truncate_width(line, max_width));
    }
    PanelContent::Text(std::borrow::Cow::Owned(output))
}

fn render_menu(state: &AppState, max_lines: usize) -> PanelContent<'_> {
    if state.menu_items.is_empty() {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No options available"));
    }

    let visible_count = max_lines.max(1);
    let scroll = list_scroll_offset(state.menu_selected, state.menu_items.len(), visible_count);
    let end = (scroll + visible_count).min(state.menu_items.len());
    let visible = &state.menu_items[scroll..end];

    let mut output = String::with_capacity(visible_count * 32);
    for (local_i, item) in visible.iter().enumerate() {
        let global_i = scroll + local_i;
        let prefix = if global_i == state.menu_selected { "> " } else { "  " };
        let _ = writeln!(output, "{}{}", prefix, item);
    }
    PanelContent::Text(std::borrow::Cow::Owned(output))
}

fn render_leader_help(state: &AppState, max_lines: usize, _max_width: usize) -> PanelContent<'_> {
    let mut lines = Vec::with_capacity(24);
    lines.push("Leader Actions".to_string());
    lines.push(String::new());

    // Context-sensitive actions
    let panel = state.focused_panel;
    let mut context_lines = Vec::new();
    match panel {
        PanelId::Containers => {
            context_lines.push(("e", "Toggle hide stopped containers"));
            context_lines.push(("p", "Pause / unpause container"));
            context_lines.push(("c", "Custom container commands"));
        }
        PanelId::Projects => {
            context_lines.push(("e", "Edit docker-compose.yml"));
            context_lines.push(("o", "Open docker-compose.yml"));
            context_lines.push(("c", "Custom project commands"));
        }
        PanelId::Services => {
            context_lines.push(("S", "Start service"));
            context_lines.push(("c", "Custom service commands"));
        }
        PanelId::Images => {
            context_lines.push(("c", "Custom image commands"));
            context_lines.push(("p", "Prune unused images"));
        }
        PanelId::Volumes => {
            context_lines.push(("c", "Custom volume commands"));
            context_lines.push(("p", "Prune unused volumes"));
        }
        PanelId::Networks => {
            context_lines.push(("c", "Custom network commands"));
            context_lines.push(("p", "Prune unused networks"));
        }
        _ => {}
    }
    if !context_lines.is_empty() {
        lines.push(format!("Context ({panel:?})"));
        for (key, desc) in context_lines {
            lines.push(format!("  {key:<9} {desc}"));
        }
        lines.push(String::new());
    }

    lines.push("Actions".to_string());
    let actions = [
        ("s", "Stop"),
        ("u", "Start"),
        ("r", "Restart"),
        ("d", "Remove / delete"),
        ("R", "Restart policy"),
        ("E", "Exec shell"),
        ("a", "Attach"),
        ("w", "Open in browser"),
        ("x", "Options menu"),
        ("b", "Bulk operations"),
        ("U", "Project up"),
        ("D", "Project down"),
        ("m", "View logs"),
        ("X", "Global custom commands"),
    ];
    for (key, desc) in actions {
        lines.push(format!("  {key:<9} {desc}"));
    }
    lines.push(String::new());
    lines.push("Navigation".to_string());
    let nav = [
        ("j / k", "Move down / up"),
        ("Tab", "Focus next panel"),
        ("[ / ]", "Focus previous / next"),
        ("1-6", "Focus panel directly"),
        ("l / S / c / e", "Logs / Stats / Config / Env"),
        ("H / L", "Scroll left / right"),
        ("J / K", "Scroll large up / down"),
        ("+ / _", "Screen mode"),
        ("q", "Quit"),
        ("h / ?", "Help"),
        ("/", "Search"),
    ];
    for (key, desc) in nav {
        lines.push(format!("  {key:<9} {desc}"));
    }

    // Leader help is short and self-contained; do not inherit scroll from
    // any other context (previously incorrectly used MainContext::Logs).
    let scroll = 0;
    let max_scroll = lines.len().saturating_sub(max_lines);
    let scroll = scroll.min(max_scroll);
    let end = (scroll + max_lines).min(lines.len());
    let visible = if scroll >= lines.len() {
        &[] as &[String]
    } else {
        &lines[scroll..end]
    };
    PanelContent::Text(std::borrow::Cow::Owned(visible.join("
")))
}

fn render_main_panel(state: &AppState, max_lines: usize, max_width: usize) -> PanelContent<'_> {
    let h_scroll = state
        .horizontal_scroll_offsets
        .get(&state.active_main_context)
        .copied()
        .unwrap_or(0);

    if state.input_mode == InputMode::Leader {
        return render_leader_help(state, max_lines, max_width);
    }

    if let (InputMode::Confirm, Some((_, prompt))) = (state.input_mode, &state.pending_confirmation) {
        let lines = [
            prompt.as_str(),
            "",
            "Press y to confirm, n or Esc to cancel",
        ];
        let end = max_lines.min(lines.len());
        let visible = &lines[..end];
        return PanelContent::Text(std::borrow::Cow::Owned(visible.join("
")));
    }

    if state.input_mode == InputMode::Menu {
        return render_menu(state, max_lines);
    }

    let content = if state.input_mode == InputMode::Help {
        render_help_text(max_lines, max_width)
    } else {
        match state.active_main_context {
            MainContext::Logs => {
                let search_prompt_lines = if state.input_mode == InputMode::Search {
                    2
                } else {
                    0
                };
                let available_lines = max_lines.saturating_sub(search_prompt_lines).max(1);

                let filter = state.log_filter.as_deref().and_then(|f| {
                    if f.is_empty() {
                        None
                    } else {
                        Some(f)
                    }
                });

                let (total_lines, visible_iter): (_, Box<dyn Iterator<Item = &(String, LogStream)>>) =
                    if filter.is_none() {
                        let total = state.log_buffer.len();
                        let iter: Box<dyn Iterator<Item = &(String, LogStream)>> =
                            Box::new(state.log_buffer.iter());
                        (total, iter)
                    } else {
                        let mut lines = Vec::with_capacity(state.log_buffer.len().min(available_lines));
                        for entry in state.log_buffer.iter() {
                            if let Some(filter) = filter
                                && !contains_ignore_case(&entry.0, filter)
                            {
                                continue;
                            }
                            if is_shell_prompt(&entry.0) {
                                continue;
                            }
                            lines.push(entry);
                        }
                        let total = lines.len();
                        let iter: Box<dyn Iterator<Item = &(String, LogStream)>> =
                            Box::new(lines.into_iter());
                        (total, iter)
                    };

                let max_scroll = total_lines.saturating_sub(available_lines);
                let raw_scroll = state
                    .main_scroll_offsets
                    .get(&MainContext::Logs)
                    .copied()
                    .unwrap_or(0);
                let scroll = if state.logs_follow_bottom {
                    max_scroll
                } else if state.scroll_past_bottom {
                    raw_scroll
                } else {
                    raw_scroll.min(max_scroll)
                };
                let end = (scroll + available_lines).min(total_lines);
                let visible = visible_iter.skip(scroll).take(end.saturating_sub(scroll));

                let mut text_lines: Vec<Line<'_>> =
                    Vec::with_capacity(search_prompt_lines + (end - scroll) + 1);

                if state.input_mode == InputMode::Search {
                    let prompt = state.log_filter.as_deref().unwrap_or("");
                    text_lines.push(Line::from(format!("/{prompt}")));
                    text_lines.push(Line::from("-".repeat(prompt.len().saturating_add(1))));
                }

                let mut visible_count = 0;
                for (text, stream) in visible {
                    visible_count += 1;
                    let style = match stream {
                        LogStream::Stderr => Style::default().fg(Color::Red),
                        _ => Style::default(),
                    };
                    text_lines.push(Line::styled(text.as_str(), style));
                }

                if visible_count == 0 {
                    let msg = if state.log_buffer.is_empty() {
                        "Waiting for logs..."
                    } else {
                        "No logs match current filter"
                    };
                    text_lines.push(Line::from(msg));
                }

                PanelContent::StyledText(Text::from(text_lines))
            }
            MainContext::Stats => {
                if let Some(stats) = &state.active_stats {
                    let mut output = String::with_capacity(256);
                    let _ = writeln!(output, "CPU:    {:.2}%", stats.cpu_percent);
                    let _ = writeln!(
                        output,
                        "Memory: {} / {} ({:.1}%)",
                        format_bytes(stats.memory_usage),
                        format_bytes(stats.memory_limit),
                        if stats.memory_limit > 0 {
                            (stats.memory_usage as f64 / stats.memory_limit as f64) * 100.0
                        } else {
                            0.0
                        }
                    );
                    if !state.stats_history.is_empty() {
                        let _ = writeln!(output, "\nCPU History:");
                        output.push_str(&render_sparkline(&state.stats_history, max_width));
                    }
                    if !state.memory_history.is_empty() {
                        let _ = writeln!(output, "\nMemory History:");
                        output.push_str(&render_sparkline(&state.memory_history, max_width));
                    }
                    PanelContent::Text(std::borrow::Cow::Owned(output))
                } else {
                    PanelContent::Text(std::borrow::Cow::Borrowed("Waiting for stats..."))
                }
            }
            MainContext::Config => {
                if let Some(container) = state.selected_container() {
                    let name = container.names.first().map(|s| s.as_str()).unwrap_or(&container.id);
                    let mut lines = Vec::with_capacity(10);
                    lines.push(format!("Config for {name}"));
                    lines.push(String::new());
                    lines.push(format!("ID:     {}", container.id));
                    lines.push(format!("Image:  {}", container.image));
                    if let Some(st) = &container.state {
                        lines.push(format!("State:  {st}"));
                    }
                    if let Some(status) = &container.status {
                        lines.push(format!("Status: {status}"));
                    }
                    if let Some(project) = &container.compose_project {
                        lines.push(String::new());
                        lines.push(format!("Compose Project: {project}"));
                    }
                    if let Some(service) = &container.compose_service {
                        lines.push(format!("Compose Service: {service}"));
                    }
                    let scroll = state
                        .main_scroll_offsets
                        .get(&MainContext::Config)
                        .copied()
                        .unwrap_or(0);
                    let max_scroll = lines.len().saturating_sub(max_lines);
                    let scroll = if state.scroll_past_bottom {
                        scroll
                    } else {
                        scroll.min(max_scroll)
                    };
                    let end = (scroll + max_lines).min(lines.len());
                    let visible = if scroll >= lines.len() {
                        &[] as &[String]
                    } else {
                        &lines[scroll..end]
                    };
                    PanelContent::Text(std::borrow::Cow::Owned(visible.join("
")))
                } else {
                    PanelContent::Text(std::borrow::Cow::Borrowed("No container selected"))
                }
            }
            MainContext::Env => {
                if let Some(container) = state.selected_container() {
                    if state.env_vars.is_empty() {
                        PanelContent::Text(if state.docker.is_available() {
                            std::borrow::Cow::Borrowed("Loading environment variables...")
                        } else {
                            std::borrow::Cow::Borrowed("Docker unavailable")
                        })
                    } else {
                        let name = container.names.first().map(|s| s.as_str()).unwrap_or(&container.id);
                        let mut lines = Vec::with_capacity(state.env_vars.len() + 2);
                        lines.push(format!("Environment variables for {name}:"));
                        lines.push(String::new());
                        for (key, value) in &state.env_vars {
                            lines.push(format!("{key}={value}"));
                        }
                        let scroll = state
                            .main_scroll_offsets
                            .get(&MainContext::Env)
                            .copied()
                            .unwrap_or(0);
                        let max_scroll = lines.len().saturating_sub(max_lines);
                        let scroll = if state.scroll_past_bottom {
                            scroll
                        } else {
                            scroll.min(max_scroll)
                        };
                        let end = (scroll + max_lines).min(lines.len());
                        let visible = if scroll >= lines.len() {
                            &[] as &[String]
                        } else {
                            &lines[scroll..end]
                        };
                        PanelContent::Text(std::borrow::Cow::Owned(visible.join("
")))
                    }
                } else {
                    PanelContent::Text(std::borrow::Cow::Borrowed("No container selected"))
                }
            }
            MainContext::ImageInfo => {
                if let Some(image) = state.selected_image() {
                    let mut lines = Vec::with_capacity(10);
                    lines.push(format!("Image: {}", image.repo_tags.first().unwrap_or(&image.id)));
                    lines.push(String::new());
                    lines.push(format!("ID:          {}", image.id));
                    lines.push(format!("Size:        {}", format_bytes(image.size as u64)));
                    if image.created > 0 {
                        lines.push(format!("Created:     {}", image.created));
                    }
                    if image.repo_tags.len() > 1 {
                        lines.push(String::new());
                        lines.push("Tags:".to_string());
                        for tag in &image.repo_tags {
                            lines.push(format!("  {tag}"));
                        }
                    }
                    let scroll = state
                        .main_scroll_offsets
                        .get(&MainContext::ImageInfo)
                        .copied()
                        .unwrap_or(0);
                    let max_scroll = lines.len().saturating_sub(max_lines);
                    let scroll = if state.scroll_past_bottom {
                        scroll
                    } else {
                        scroll.min(max_scroll)
                    };
                    let end = (scroll + max_lines).min(lines.len());
                    let visible = if scroll >= lines.len() {
                        &[] as &[String]
                    } else {
                        &lines[scroll..end]
                    };
                    PanelContent::Text(std::borrow::Cow::Owned(visible.join("
")))
                } else {
                    PanelContent::Text(std::borrow::Cow::Borrowed("No image selected"))
                }
            }
            MainContext::VolumeInfo => {
                if let Some(volume) = state.selected_volume() {
                    let mut lines = Vec::with_capacity(6);
                    lines.push(format!("Volume: {}", volume.name));
                    lines.push(String::new());
                    lines.push(format!("Driver:    {}", volume.driver));
                    lines.push(format!("Mountpoint: {}", volume.mountpoint));
                    if let Some(created) = &volume.created_at {
                        lines.push(format!("Created:   {}", created));
                    }
                    let scroll = state
                        .main_scroll_offsets
                        .get(&MainContext::VolumeInfo)
                        .copied()
                        .unwrap_or(0);
                    let max_scroll = lines.len().saturating_sub(max_lines);
                    let scroll = if state.scroll_past_bottom {
                        scroll
                    } else {
                        scroll.min(max_scroll)
                    };
                    let end = (scroll + max_lines).min(lines.len());
                    let visible = if scroll >= lines.len() {
                        &[] as &[String]
                    } else {
                        &lines[scroll..end]
                    };
                    PanelContent::Text(std::borrow::Cow::Owned(visible.join("
")))
                } else {
                    PanelContent::Text(std::borrow::Cow::Borrowed("No volume selected"))
                }
            }
            MainContext::NetworkInfo => {
                if let Some(network) = state.selected_network() {
                    let mut lines = Vec::with_capacity(6);
                    lines.push(format!("Network: {}", network.name));
                    lines.push(String::new());
                    lines.push(format!("ID:       {}", network.id));
                    lines.push(format!("Driver:   {}", network.driver));
                    if let Some(created) = &network.created {
                        lines.push(format!("Created:  {}", created));
                    }
                    let scroll = state
                        .main_scroll_offsets
                        .get(&MainContext::NetworkInfo)
                        .copied()
                        .unwrap_or(0);
                    let max_scroll = lines.len().saturating_sub(max_lines);
                    let scroll = if state.scroll_past_bottom {
                        scroll
                    } else {
                        scroll.min(max_scroll)
                    };
                    let end = (scroll + max_lines).min(lines.len());
                    let visible = if scroll >= lines.len() {
                        &[] as &[String]
                    } else {
                        &lines[scroll..end]
                    };
                    PanelContent::Text(std::borrow::Cow::Owned(visible.join("
")))
                } else {
                    PanelContent::Text(std::borrow::Cow::Borrowed("No network selected"))
                }
            }
        }
    };

    if h_scroll == 0 {
        return content;
    }

    match content {
        PanelContent::Text(text) => {
            PanelContent::Text(apply_horizontal_scroll_text(&text, h_scroll))
        }
        PanelContent::StyledText(text) => {
            PanelContent::StyledText(apply_horizontal_scroll_styled(text, h_scroll))
        }
        PanelContent::Table { .. } => content,
    }
}

fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;

    let mut buf = String::with_capacity(16);
    if bytes < KB {
        let _ = write!(buf, "{} B", bytes);
    } else if bytes < MB {
        let _ = write!(buf, "{:.2} KB", bytes as f64 / KB as f64);
    } else if bytes < GB {
        let _ = write!(buf, "{:.2} MB", bytes as f64 / MB as f64);
    } else {
        let _ = write!(buf, "{:.2} GB", bytes as f64 / GB as f64);
    }
    buf
}

/// Truncate a string to fit within `max_width` display columns, respecting
/// multi-byte UTF-8 characters and wide characters (e.g. CJK, emoji).
/// Appends `".."` if truncation occurred.
pub fn truncate_width(s: &str, max_width: usize) -> Cow<'_, str> {
    if max_width == 0 {
        return Cow::Borrowed("");
    }
    if max_width <= 2 {
        return Cow::Borrowed("..");
    }

    let mut width = 0usize;
    let mut split_at = 0usize;
    for (idx, ch) in s.char_indices() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > max_width.saturating_sub(2) {
            split_at = idx;
            break;
        }
        width += ch_width;
        split_at = idx + ch.len_utf8();
    }

    if split_at < s.len() {
        Cow::Owned(format!("{}..", &s[..split_at]))
    } else {
        Cow::Borrowed(s)
    }
}

/// Skip the first `skip_width` display columns of a string, returning the remainder.
fn skip_width(s: &str, skip_width: usize) -> &str {
    if skip_width == 0 {
        return s;
    }
    let mut width = 0usize;
    for (idx, ch) in s.char_indices() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if width + ch_width > skip_width {
            return &s[idx..];
        }
        width += ch_width;
    }
    ""
}

fn is_shell_prompt(line: &str) -> bool {
    let trimmed = line.trim_start();

    // Pattern: user@host:path# or user@host:path$
    // Allow path prefixes like /root, ~user, ./user before @
    if let Some(at_pos) = trimmed.find('@') {
        let user = &trimmed[..at_pos];
        if !user.is_empty()
            && user.chars().all(|c| {
                c.is_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.' || c == '~'
            })
        {
            let rest = &trimmed[at_pos + 1..];
            for (i, ch) in rest.char_indices() {
                if ch == '#' || ch == '$' || ch == '%' || ch == '>' {
                    let before = &rest[..i];
                    if before.is_empty()
                        || before.chars().all(|c| {
                            c.is_alphanumeric()
                                || c == '-'
                                || c == '_'
                                || c == ':'
                                || c == '/'
                                || c == '.'
                                || c == '~'
                        })
                    {
                        return true;
                    }
                }
            }
        }
    }

    // Simple prompt at start of line
    if trimmed.starts_with("# ")
        || trimmed.starts_with("$ ")
        || trimmed.starts_with("% ")
        || trimmed.starts_with("> ")
    {
        return true;
    }
    if trimmed == "#" || trimmed == "$" || trimmed == "%" || trimmed == ">" {
        return true;
    }

    false
}

#[cfg(test)]
fn hard_wrap_line(line: &str, max_width: usize) -> Vec<String> {
    if max_width == 0 {
        return vec![line.to_string()];
    }

    // Expand tabs to spaces to get accurate width
    let expanded = line.replace('\t', "        ");

    let mut result = Vec::new();
    let mut current = String::new();
    let mut width = 0;

    for ch in expanded.chars() {
        let ch_width = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch_width == 0 {
            // Zero-width characters (combining marks, ZWJ, etc.) stay with
            // the current segment so they aren't dropped.
            current.push(ch);
            continue;
        }
        if width + ch_width > max_width && !current.is_empty() {
            result.push(current);
            current = String::new();
            width = 0;
        }
        current.push(ch);
        width += ch_width;
    }

    if !current.is_empty() || result.is_empty() {
        result.push(current);
    }

    result
}

fn apply_horizontal_scroll_text(text: &str, h_scroll: usize) -> std::borrow::Cow<'static, str> {
    if h_scroll == 0 {
        return std::borrow::Cow::Owned(text.to_string());
    }
    std::borrow::Cow::Owned(
        text.lines()
            .map(|line| skip_width(line, h_scroll))
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

fn apply_horizontal_scroll_styled<'a>(text: Text<'a>, h_scroll: usize) -> Text<'a> {
    if h_scroll == 0 {
        return text;
    }
    let mut new_lines = Vec::with_capacity(text.lines.len());
    for line in text.lines {
        new_lines.push(truncate_line_left(line, h_scroll));
    }
    Text::from(new_lines)
}

fn truncate_line_left(line: Line<'_>, h_scroll: usize) -> Line<'static> {
    let mut remaining = h_scroll;
    let mut new_spans = Vec::new();

    for span in line.spans {
        let span_text = span.content.as_ref();
        let span_width = unicode_width::UnicodeWidthStr::width(span_text);

        if remaining >= span_width {
            remaining -= span_width;
            continue;
        }

        if remaining == 0 {
            new_spans.push(Span::styled(span.content.to_string(), span.style));
        } else {
            let mut skipped_width = 0;
            let mut char_count = 0;
            for ch in span_text.chars() {
                let ch_w = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                if skipped_width + ch_w > remaining {
                    break;
                }
                skipped_width += ch_w;
                char_count += 1;
            }
            let remaining_text: String = span_text.chars().skip(char_count).collect();
            if !remaining_text.is_empty() {
                new_spans.push(Span::styled(remaining_text, span.style));
            }
            remaining = 0;
        }
    }

    Line::from(new_spans)
}

fn render_sparkline(data: &std::collections::VecDeque<f64>, max_width: usize) -> String {
    if data.is_empty() || max_width == 0 {
        return String::new();
    }
    let max = data.iter().copied().fold(0.0f64, f64::max).max(1.0);
    let blocks = [" ", "▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let start = data.len().saturating_sub(max_width);
    data
        .iter()
        .skip(start)
        .map(|v| {
            let idx = ((v / max) * (blocks.len() - 1) as f64).round() as usize;
            blocks[idx.min(blocks.len() - 1)]
        })
        .collect::<String>()
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

fn status_content(state: &AppState, max_width: usize) -> String {
    let mut buf = String::with_capacity(120);
    if let Some((_, prompt)) = &state.pending_confirmation {
        let _ = write!(buf, "[Confirm] {prompt} (y/n)");
    } else if let Some(message) = &state.error_message {
        let _ = write!(buf, "Error: {message}");
    } else if state.input_mode == InputMode::Leader {
        buf.push_str("[Leader] Press a key or Esc to cancel");
    } else if let Some((message, _)) = &state.status_message {
        buf.push_str(message);
    } else {
        match &state.docker {
            DockerState::Unknown => {
                buf.push_str("docker: connecting...");
            }
            DockerState::Available(info) => {
                let version = info.server_version.as_deref().unwrap_or("unknown");
                let _ = write!(
                    buf,
                    "docker: {version} | {} containers",
                    state.containers.len()
                );
            }
            DockerState::Unavailable(message) => {
                let _ = write!(buf, "docker: unavailable ({message})");
            }
        }
        buf.push_str(" | ? help | q quit");
    }
    truncate_width(&buf, max_width).into_owned()
}

fn container_row_style(container: &crate::docker::ContainerItem) -> Style {
    match container.state.as_deref() {
        Some("running") => Style::default().fg(Color::Green),
        Some("exited") | Some("dead") => Style::default().fg(Color::Red),
        Some("paused") => Style::default().fg(Color::Yellow),
        Some("created") | Some("restarting") => Style::default().fg(Color::Cyan),
        _ => Style::default(),
    }
}

fn render_container_list<'a>(
    containers: &'a [crate::docker::ContainerItem],
    selected_index: usize,
    filter: Option<&'a str>,
    max_width: u16,
    max_lines: u16,
) -> PanelContent<'a> {
    let is_filtered = filter.is_some_and(|f| !f.is_empty());
    let filtered_indices: Vec<usize> = if is_filtered {
        containers
            .iter()
            .enumerate()
            .filter(|(_, c)| container_matches(c, filter.unwrap()))
            .map(|(i, _)| i)
            .collect()
    } else {
        Vec::new()
    };

    if containers.is_empty() {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No containers found"));
    }
    let total = if is_filtered { filtered_indices.len() } else { containers.len() };
    if total == 0 {
        return PanelContent::Text(std::borrow::Cow::Borrowed("No containers match filter"));
    }

    let filtered_selected = if is_filtered {
        filtered_indices
            .iter()
            .position(|&i| i == selected_index)
            .unwrap_or(0)
    } else {
        selected_index.min(total.saturating_sub(1))
    };

    let content_width = max_width.saturating_sub(4); // 2 borders + 2 padding
    let compact = content_width < 45;
    let header_lines = if max_lines > 2 { 1 } else { 0 };
    let visible_count = max_lines.saturating_sub(header_lines).max(1) as usize;
    let scroll = list_scroll_offset(filtered_selected, total, visible_count);
    let end = (scroll + visible_count).min(total);

    let mut rows = Vec::with_capacity(end.saturating_sub(scroll));
    let mut row_styles = Vec::with_capacity(end.saturating_sub(scroll));

    let mut add_row = |idx: usize| {
        let container = &containers[idx];
        let name = container.names.first().map(|s| s.as_str()).unwrap_or("");
        let status = container.status.as_deref().unwrap_or("-");

        if compact {
            let name_w = (content_width as usize).saturating_sub(10).max(1);
            let status_short = status.split_whitespace().next().unwrap_or("-");
            rows.push(vec![
                truncate_width(name, name_w),
                truncate_width(status_short, 8),
            ]);
            row_styles.push(container_row_style(container));
        } else {
            rows.push(vec![
                truncate_width(name, 28),
                truncate_width(&container.image, 28),
                truncate_width(status, 18),
            ]);
            row_styles.push(container_row_style(container));
        }
    };
    if is_filtered {
        for &idx in &filtered_indices[scroll..end] {
            add_row(idx);
        }
    } else {
        for idx in scroll..end {
            add_row(idx);
        }
    }

    if compact {
        let name_w = (content_width as usize).saturating_sub(10).max(1);
        PanelContent::Table {
            header: if header_lines > 0 {
                Some(vec!["NAME", "STATUS"])
            } else {
                None
            },
            rows,
            row_styles,
            selected: filtered_selected.saturating_sub(scroll),
            offset: 0,
            widths: vec![
                Constraint::Min(name_w as u16),
                Constraint::Length(8),
            ],
        }
    } else {
        PanelContent::Table {
            header: if header_lines > 0 {
                Some(vec!["NAME", "IMAGE", "STATUS"])
            } else {
                None
            },
            rows,
            row_styles,
            selected: filtered_selected.saturating_sub(scroll),
            offset: 0,
            widths: vec![
                Constraint::Percentage(33),
                Constraint::Percentage(33),
                Constraint::Percentage(34),
            ],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_width_ascii() {
        assert_eq!(truncate_width("hello", 10), "hello");
        assert_eq!(truncate_width("hello world", 8), "hello ..");
    }

    #[test]
    fn truncate_width_unicode() {
        // CJK chars are width 2; "日本語テ" = 8 cols + ".." = 10 cols
        assert_eq!(truncate_width("日本語テキスト", 10), "日本語テ..");
        // Emoji is width 2; "🎉 p" = 4 cols + ".." = 6 cols
        assert_eq!(truncate_width("🎉 party", 6), "🎉 p..");
    }

    #[test]
    fn truncate_width_wide_chars() {
        // CJK characters are width 2
        assert_eq!(truncate_width("中文内容", 6), "中文..");
    }

    #[test]
    fn truncate_width_edge_cases() {
        assert_eq!(truncate_width("", 5), "");
        assert_eq!(truncate_width("abc", 0), "");
        assert_eq!(truncate_width("abc", 2), "..");
    }

    #[test]
    fn dynamic_title_shows_selection_count() {
        let spec = PanelSpec::new(PanelId::Containers, true, "Containers");
        let state = AppState {
            containers: vec![
                crate::docker::ContainerItem {
                    id: "1".to_string(),
                    names: vec!["a".to_string()],
                    image: "img".to_string(),
                    state: None,
                    status: None,
                    compose_project: None,
                    compose_service: None,
                    compose_container_number: None,
                    compose_oneoff: false,
                    compose_working_dir: None,
                },
                crate::docker::ContainerItem {
                    id: "2".to_string(),
                    names: vec!["b".to_string()],
                    image: "img".to_string(),
                    state: None,
                    status: None,
                    compose_project: None,
                    compose_service: None,
                    compose_container_number: None,
                    compose_oneoff: false,
                    compose_working_dir: None,
                },
            ],
            ..AppState::default()
        };
        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Containers (1/2)"));
    }

    #[test]
    fn dynamic_title_fallback_for_empty_lists() {
        let spec = PanelSpec::new(PanelId::Containers, true, "Containers");
        let state = AppState::default();
        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Containers"));
    }

    #[test]
    fn main_panel_title_shows_context_and_container_name() {
        let spec = PanelSpec::new(PanelId::Main, true, "Main");
        let mut state = AppState {
            active_main_context: MainContext::Logs,
            ..AppState::default()
        };
        state.containers = vec![crate::docker::ContainerItem {
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
        }];
        state.selected_indexes.insert(PanelId::Containers, 0);

        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Logs: web-server"));

        state.active_main_context = MainContext::Config;
        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Config: web-server"));

        state.active_main_context = MainContext::Env;
        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Env: web-server"));
    }

    #[test]
    fn main_panel_title_shows_cpu_when_stats_available() {
        let spec = PanelSpec::new(PanelId::Main, true, "Main");
        let mut state = AppState {
            active_main_context: MainContext::Stats,
            ..AppState::default()
        };
        state.containers = vec![crate::docker::ContainerItem {
            id: "c1".to_string(),
            names: vec!["redis".to_string()],
            image: "redis".to_string(),
            state: None,
            status: None,
            compose_project: None,
            compose_service: None,
            compose_container_number: None,
            compose_oneoff: false,
                compose_working_dir: None,
        }];
        state.selected_indexes.insert(PanelId::Containers, 0);

        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Stats: redis"));

        state.active_stats = Some(crate::docker::ContainerStatsSample {
            container_id: "c1".to_string(),
            cpu_percent: 42.5,
            memory_usage: 100,
            memory_limit: 1000,
        });
        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Stats: redis (42.5% CPU)"));
    }

    #[test]
    fn main_panel_title_fallback_when_no_container_selected() {
        let spec = PanelSpec::new(PanelId::Main, true, "Main");
        let state = AppState {
            active_main_context: MainContext::Logs,
            ..AppState::default()
        };
        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Logs: ?"));
    }

    #[test]
    fn container_list_renders_compact_in_narrow_width() {
        let containers = vec![
            crate::docker::ContainerItem {
                id: "1".to_string(),
                names: vec!["web-server".to_string()],
                image: "nginx:latest".to_string(),
                state: Some("running".to_string()),
                status: Some("Up 2 minutes".to_string()),
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
        },
        ];
        let compact = render_container_list(&containers, 0, None, 30, 20);
        match compact {
            PanelContent::Table { header, rows, selected, offset, .. } => {
                assert_eq!(header, Some(vec!["NAME", "STATUS"]));
                assert!(!rows.is_empty());
                assert!(rows[0].iter().any(|cell| cell.contains("Up")));
                assert_eq!(selected, 0);
                assert_eq!(offset, 0);
            }
            _ => panic!("Expected PanelContent::Table"),
        }
    }

    #[test]
    fn container_list_renders_full_in_wide_width() {
        let containers = vec![
            crate::docker::ContainerItem {
                id: "1".to_string(),
                names: vec!["web-server".to_string()],
                image: "nginx:latest".to_string(),
                state: Some("running".to_string()),
                status: Some("Up 2 minutes".to_string()),
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
        },
        ];
        let full = render_container_list(&containers, 0, None, 100, 20);
        match full {
            PanelContent::Table { header, rows, selected, offset, .. } => {
                assert_eq!(header, Some(vec!["NAME", "IMAGE", "STATUS"]));
                assert!(!rows.is_empty());
                assert!(rows.iter().any(|row| row.iter().any(|cell| cell.contains("nginx:latest"))));
                assert_eq!(selected, 0);
                assert_eq!(offset, 0);
            }
            _ => panic!("Expected PanelContent::Table"),
        }
    }

    #[test]
    fn container_list_scrolls_to_keep_selection_visible() {
        let containers: Vec<_> = (0..20)
            .map(|i| crate::docker::ContainerItem {
                id: format!("{i}"),
                names: vec![format!("container-{i}")],
                image: "img".to_string(),
                state: None,
                status: None,
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            })
            .collect();

        let output = render_container_list(&containers, 15, None, 100, 5);
        match output {
            PanelContent::Table { rows, selected, offset, .. } => {
                // max_lines=5, header_lines=1, visible_count=4
                // scroll = list_scroll_offset(15, 20, 4) = 13
                // selected = 15 - 13 = 2
                assert_eq!(selected, 2);
                assert_eq!(offset, 0);
                assert_eq!(rows.len(), 4);
                assert!(rows.iter().any(|row| row.iter().any(|cell| cell.contains("container-15"))));
                assert!(!rows.iter().any(|row| row.iter().any(|cell| cell.contains("container-0"))));
            }
            _ => panic!("Expected PanelContent::Table"),
        }
    }

    #[test]
    fn log_panel_colors_stderr_red() {
        use crate::docker::LogStream;
        use crate::state::{AppState, MainContext};

        let mut state = AppState {
            active_main_context: MainContext::Logs,
            ..AppState::default()
        };
        state.log_buffer.push_back(("stdout line".to_string(), LogStream::Stdout));
        state.log_buffer.push_back(("stderr line".to_string(), LogStream::Stderr));
        state.log_buffer.push_back(("console line".to_string(), LogStream::Console));

        let content = render_main_panel(&state, 10, 80);
        match content {
            PanelContent::StyledText(text) => {
                assert_eq!(text.lines.len(), 3);
                // stdout should have default style (no red)
                assert_eq!(text.lines[0].style, Style::default());
                // stderr should have red foreground
                assert_eq!(text.lines[1].style.fg, Some(Color::Red));
                // console should have default style
                assert_eq!(text.lines[2].style, Style::default());
            }
            _ => panic!("Expected PanelContent::StyledText"),
        }
    }

    #[test]
    fn log_panel_shows_search_prompt_in_search_mode() {
        use crate::docker::LogStream;
        use crate::state::{AppState, InputMode, MainContext};

        let mut state = AppState {
            active_main_context: MainContext::Logs,
            input_mode: InputMode::Search,
            log_filter: Some("err".to_string()),
            ..AppState::default()
        };
        state.log_buffer.push_back(("error: fail".to_string(), LogStream::Stderr));

        let content = render_main_panel(&state, 10, 80);
        match content {
            PanelContent::StyledText(text) => {
                assert!(text.lines.len() >= 2);
                assert!(text.lines[0].to_string().contains("/err"));
                assert!(text.lines[1].to_string().contains("---"));
            }
            _ => panic!("Expected PanelContent::StyledText"),
        }
    }

    #[test]
    fn scroll_past_bottom_allows_scrolling_beyond_content() {
        use crate::docker::LogStream;
        use crate::state::{AppState, MainContext};

        let mut state = AppState {
            active_main_context: MainContext::Logs,
            scroll_past_bottom: true,
            ..AppState::default()
        };
        state.log_buffer.push_back(("line1".to_string(), LogStream::Stdout));
        state
            .main_scroll_offsets
            .insert(MainContext::Logs, 100);

        let content = render_main_panel(&state, 10, 80);
        match content {
            PanelContent::StyledText(text) => {
                // When scrolled past content, visible is empty but rendering shows
                // "No logs match current filter" since log_buffer is not empty
                assert!(!text.lines.is_empty());
            }
            _ => panic!("Expected PanelContent::StyledText"),
        }
    }

    #[test]
    fn scroll_clamped_without_scroll_past_bottom() {
        use crate::docker::LogStream;
        use crate::state::{AppState, MainContext};

        let mut state = AppState {
            active_main_context: MainContext::Logs,
            scroll_past_bottom: false,
            ..AppState::default()
        };
        state.log_buffer.push_back(("line1".to_string(), LogStream::Stdout));
        state
            .main_scroll_offsets
            .insert(MainContext::Logs, 100);

        let content = render_main_panel(&state, 10, 80);
        match content {
            PanelContent::StyledText(text) => {
                // Should still show the line (clamped to max_scroll=0)
                assert!(!text.lines.is_empty());
            }
            _ => panic!("Expected PanelContent::StyledText"),
        }
    }

    #[test]
    fn help_panel_renders_shortcuts() {
        use crate::state::{AppState, InputMode};

        let state = AppState {
            input_mode: InputMode::Help,
            ..AppState::default()
        };
        let content = render_main_panel(&state, 55, 80);
        match content {
            PanelContent::Text(text) => {
                assert!(text.contains("Keyboard Shortcuts"));
                assert!(text.contains("j / ↑"));
                assert!(text.contains("Quit"));
            }
            _ => panic!("Expected PanelContent::Text"),
        }
    }

    #[test]
    fn main_panel_title_shows_help_in_help_mode() {
        let spec = PanelSpec::new(PanelId::Main, true, "Main");
        let state = AppState {
            input_mode: InputMode::Help,
            ..AppState::default()
        };
        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Help"));
    }

    #[test]
    fn container_list_colors_rows_by_state() {
        let containers = vec![
            crate::docker::ContainerItem {
                id: "1".to_string(),
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
            crate::docker::ContainerItem {
                id: "2".to_string(),
                names: vec!["db".to_string()],
                image: "postgres".to_string(),
                state: Some("exited".to_string()),
                status: Some("Exited".to_string()),
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
            crate::docker::ContainerItem {
                id: "3".to_string(),
                names: vec!["cache".to_string()],
                image: "redis".to_string(),
                state: Some("paused".to_string()),
                status: Some("Paused".to_string()),
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            },
        ];
        let output = render_container_list(&containers, 0, None, 100, 20);
        match output {
            PanelContent::Table { row_styles, .. } => {
                assert_eq!(row_styles.len(), 3);
                assert_eq!(row_styles[0].fg, Some(Color::Green));
                assert_eq!(row_styles[1].fg, Some(Color::Red));
                assert_eq!(row_styles[2].fg, Some(Color::Yellow));
            }
            _ => panic!("Expected PanelContent::Table"),
        }
    }

    #[test]
    fn container_list_filters_items() {
        let containers = vec![
            crate::docker::ContainerItem {
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
            crate::docker::ContainerItem {
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
        let output = render_container_list(&containers, 0, Some("web"), 100, 20);
        match output {
            PanelContent::Table { rows, selected, .. } => {
                assert_eq!(rows.len(), 1);
                assert!(rows[0].iter().any(|cell| cell.contains("web-server")));
                assert_eq!(selected, 0);
            }
            _ => panic!("Expected PanelContent::Table"),
        }
    }

    #[test]
    fn container_list_shows_filter_empty_message() {
        let containers = vec![
            crate::docker::ContainerItem {
                id: "c1".to_string(),
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
        ];
        let output = render_container_list(&containers, 0, Some("xyz"), 100, 20);
        match output {
            PanelContent::Text(text) => {
                assert!(text.contains("No containers match filter"));
            }
            _ => panic!("Expected PanelContent::Text"),
        }
    }

    #[test]
    fn dynamic_title_shows_filtered_count() {
        let spec = PanelSpec::new(PanelId::Containers, true, "Containers");
        let mut state = AppState {
            containers: vec![
                crate::docker::ContainerItem {
                    id: "1".to_string(),
                    names: vec!["web".to_string()],
                    image: "img".to_string(),
                    state: None,
                    status: None,
                    compose_project: None,
                    compose_service: None,
                    compose_container_number: None,
                    compose_oneoff: false,
                    compose_working_dir: None,
                },
                crate::docker::ContainerItem {
                    id: "2".to_string(),
                    names: vec!["db".to_string()],
                    image: "img".to_string(),
                    state: None,
                    status: None,
                    compose_project: None,
                    compose_service: None,
                    compose_container_number: None,
                    compose_oneoff: false,
                    compose_working_dir: None,
                },
            ],
            ..AppState::default()
        };
        state.panel_filters.insert(PanelId::Containers, "web".to_string());
        // selected raw index is 0, filtered position is also 0
        state.selected_indexes.insert(PanelId::Containers, 0);
        state.warm_filtered_caches();

        let ctx = RenderContext {
            state: &state,
            focused: false,
        };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Containers (1/1 of 2)"));
    }

    #[test]
    fn skip_width_skips_display_columns() {
        assert_eq!(skip_width("hello", 2), "llo");
        assert_eq!(skip_width("hello", 0), "hello");
        assert_eq!(skip_width("hello", 10), "");
        // Wide chars: "日本語" = 6 cols
        assert_eq!(skip_width("日本語", 2), "本語");
        assert_eq!(skip_width("日本語", 6), "");
    }

    #[test]
    fn apply_horizontal_scroll_text_truncates_lines() {
        let text = "hello\nworld\nfoo";
        let scrolled = apply_horizontal_scroll_text(text, 2);
        assert_eq!(scrolled, "llo\nrld\no");
    }

    #[test]
    fn apply_horizontal_scroll_styled_preserves_styles() {
        use ratatui::text::Span;
        let text = Text::from(vec![
            Line::from(vec![
                Span::styled("error", Style::default().fg(Color::Red)),
                Span::raw(": message"),
            ]),
        ]);
        let scrolled = apply_horizontal_scroll_styled(text, 2);
        assert_eq!(scrolled.lines.len(), 1);
        // "er" (2 chars) skipped from "error", leaving "ror" in red
        assert_eq!(scrolled.lines[0].spans.len(), 2);
        assert_eq!(scrolled.lines[0].spans[0].content, "ror");
        assert_eq!(scrolled.lines[0].spans[0].style.fg, Some(Color::Red));
        assert_eq!(scrolled.lines[0].spans[1].content, ": message");
    }

    #[test]
    fn render_menu_shows_items_with_selection() {
        let mut state = AppState::default();
        state.input_mode = InputMode::Menu;
        state.menu_title = "Test Menu".to_string();
        state.menu_items = vec!["Option 1".to_string(), "Option 2".to_string(), "Option 3".to_string()];
        state.menu_selected = 1;

        let content = render_menu(&state, 10);
        match content {
            PanelContent::Text(text) => {
                assert!(text.contains("> Option 2"));
                assert!(text.contains("  Option 1"));
                assert!(text.contains("  Option 3"));
            }
            _ => panic!("Expected PanelContent::Text"),
        }
    }

    #[test]
    fn main_panel_title_shows_menu_title_in_menu_mode() {
        let spec = PanelSpec::new(PanelId::Main, true, "Main");
        let mut state = AppState::default();
        state.input_mode = InputMode::Menu;
        state.menu_title = "Remove container web".to_string();

        let ctx = RenderContext { state: &state, focused: false };
        assert_eq!(spec.dynamic_title(&ctx), Line::from("Remove container web"));
    }

    #[test]
    fn is_shell_prompt_detects_various_prompts() {
        assert!(is_shell_prompt("root@host:/# "));
        assert!(is_shell_prompt("root@host:/$ "));
        assert!(is_shell_prompt("/root@host:/# "));
        assert!(is_shell_prompt("~user@host:/$ "));
        assert!(is_shell_prompt("./user@host:~$ "));
        assert!(is_shell_prompt("user-name@host-name:/path# "));
        assert!(is_shell_prompt("# "));
        assert!(is_shell_prompt("$ "));
        assert!(is_shell_prompt("% "));
        assert!(is_shell_prompt("> "));
        assert!(is_shell_prompt("#"));
        assert!(is_shell_prompt("$"));

        assert!(!is_shell_prompt("root:x:0:0:root:/root:/bin/bash"));
        assert!(!is_shell_prompt("hello world"));
        assert!(!is_shell_prompt("2024-01-01 12:00:00 INFO message"));
    }

    #[test]
    fn hard_wrap_line_respects_max_width() {
        assert_eq!(hard_wrap_line("hello", 10), vec!["hello"]);
        assert_eq!(
            hard_wrap_line("abcdefghij", 5),
            vec!["abcde", "fghij"]
        );
        assert_eq!(
            hard_wrap_line("hello world", 8),
            vec!["hello wo", "rld"]
        );
    }

    #[test]
    fn hard_wrap_line_expands_tabs() {
        let wrapped = hard_wrap_line("a\tb", 5);
        // Tab expands to 8 spaces, so "a        b" = 10 chars
        // With max_width 5: "a    " (5 chars) and "    b" (5 chars)
        assert_eq!(wrapped.len(), 2);
        assert_eq!(wrapped[0].len(), 5);
        assert_eq!(wrapped[1].len(), 5);
    }

    #[test]
    fn hard_wrap_line_preserves_zero_width_chars() {
        // "e\u{0301}" is e + combining acute (zero width)
        let wrapped = hard_wrap_line("e\u{0301}", 1);
        assert_eq!(wrapped, vec!["e\u{0301}"]);

        // ZWJ emoji should not lose the joiner
        let wrapped = hard_wrap_line("a\u{200D}b", 2);
        assert_eq!(wrapped, vec!["a\u{200D}b"]);
    }

    #[test]
    fn hard_wrap_line_respects_tiny_width() {
        let wrapped = hard_wrap_line("abc", 1);
        // Each char (width 1) fits exactly; next char starts new line
        assert_eq!(wrapped, vec!["a", "b", "c"]);
    }

    #[test]
    fn project_list_truncates_long_names() {
        let projects = vec![crate::docker::ComposeProject {
            name: "a".repeat(200),
            services: vec![],
            working_dir: None,
        }];
        let content = render_project_list(&projects, 0, None, 20, 10);
        match content {
            PanelContent::Text(text) => {
                // Should be truncated with ".." and fit within panel width
                assert!(text.lines().next().unwrap().len() < 200);
            }
            _ => panic!("Expected PanelContent::Text"),
        }
    }

    #[test]
    fn service_list_truncates_long_names() {
        let services = vec!["a".repeat(200)];
        let content = render_service_list(&services, 0, None, 20, 10);
        match content {
            PanelContent::Text(text) => {
                assert!(text.lines().next().unwrap().len() < 200);
            }
            _ => panic!("Expected PanelContent::Text"),
        }
    }

    #[test]
    fn container_list_renders_large_unfiltered_list_without_panic() {
        let containers: Vec<_> = (0..5000)
            .map(|i| crate::docker::ContainerItem {
                id: format!("id-{i}"),
                names: vec![format!("container-{i}")],
                image: "nginx".to_string(),
                state: Some("running".to_string()),
                status: Some("Up".to_string()),
                compose_project: None,
                compose_service: None,
                compose_container_number: None,
                compose_oneoff: false,
                compose_working_dir: None,
            })
            .collect();
        let content = render_container_list(&containers, 2500, None, 80, 20);
        match content {
            PanelContent::Table { rows, selected, .. } => {
                assert!(!rows.is_empty());
                // selected=2500, scroll=centered, so selected inside visible window
                assert!(selected < rows.len());
            }
            _ => panic!("Expected PanelContent::Table"),
        }
    }

    #[test]
    fn leader_help_ignores_log_scroll_offset() {
        let mut state = AppState::default();
        state.input_mode = InputMode::Leader;
        state.main_scroll_offsets.insert(MainContext::Logs, 100);
        let content = render_main_panel(&state, 20, 60);
        match content {
            PanelContent::Text(text) => {
                // Should start from the top of leader help, not scrolled by 100
                assert!(text.starts_with("Leader Actions"));
            }
            _ => panic!("Expected PanelContent::Text"),
        }
    }
}
