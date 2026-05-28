use ratatui::layout::{Constraint, Direction, Layout, Rect};

use super::panel::PanelId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppLayout {
    projects: Option<Rect>,
    services: Option<Rect>,
    containers: Option<Rect>,
    images: Option<Rect>,
    volumes: Option<Rect>,
    networks: Option<Rect>,
    main: Rect,
    status: Option<Rect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutMode {
    Tiny,
    CompactSidebar,
    Normal,
}

impl AppLayout {
    pub fn compute(
        area: Rect,
        compact_threshold: u16,
        active_sidebar: Option<PanelId>,
        forced_mode: Option<LayoutMode>,
    ) -> Self {
        let mode = forced_mode.unwrap_or_else(|| Self::detect_mode(area, compact_threshold));
        match mode {
            LayoutMode::Tiny => Self {
                projects: None,
                services: None,
                containers: None,
                images: None,
                volumes: None,
                networks: None,
                main: area,
                status: None,
            },
            LayoutMode::CompactSidebar => {
                let status_lines = if area.height < 10 { 1 } else { 3 };
                let vertical = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(1), Constraint::Length(status_lines)])
                    .split(area);
                let horizontal = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
                    .split(vertical[0]);

                let mut layout = Self {
                    projects: None,
                    services: None,
                    containers: None,
                    images: None,
                    volumes: None,
                    networks: None,
                    main: horizontal[1],
                    status: Some(vertical[1]),
                };

                // Only the active sidebar panel gets an area
                if let Some(panel) = active_sidebar {
                    match panel {
                        PanelId::Projects => layout.projects = Some(horizontal[0]),
                        PanelId::Services => layout.services = Some(horizontal[0]),
                        PanelId::Containers => layout.containers = Some(horizontal[0]),
                        PanelId::Images => layout.images = Some(horizontal[0]),
                        PanelId::Volumes => layout.volumes = Some(horizontal[0]),
                        PanelId::Networks => layout.networks = Some(horizontal[0]),
                        _ => {}
                    }
                }
                layout
            }
            LayoutMode::Normal => {
                let status_lines = if area.height < 10 { 1 } else { 3 };
                let vertical = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Min(1), Constraint::Length(status_lines)])
                    .split(area);
                let horizontal = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Percentage(33), Constraint::Percentage(67)])
                    .split(vertical[0]);
                let side = Layout::default()
                    .direction(Direction::Vertical)
                    .constraints([Constraint::Ratio(1, 6); 6])
                    .split(horizontal[0]);
                Self {
                    projects: Some(side[0]),
                    services: Some(side[1]),
                    containers: Some(side[2]),
                    images: Some(side[3]),
                    volumes: Some(side[4]),
                    networks: Some(side[5]),
                    main: horizontal[1],
                    status: Some(vertical[1]),
                }
            }
        }
    }

    fn detect_mode(area: Rect, compact_threshold: u16) -> LayoutMode {
        if area.width < 24 || area.height < 6 {
            LayoutMode::Tiny
        } else if area.width < compact_threshold {
            LayoutMode::CompactSidebar
        } else {
            LayoutMode::Normal
        }
    }

    pub fn area(&self, panel_id: PanelId) -> Option<Rect> {
        match panel_id {
            PanelId::Projects => self.projects,
            PanelId::Services => self.services,
            PanelId::Containers => self.containers,
            PanelId::Images => self.images,
            PanelId::Volumes => self.volumes,
            PanelId::Networks => self.networks,
            PanelId::Main => Some(self.main),
            PanelId::Status => self.status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn computes_all_core_areas_for_normal_terminal() {
        let layout = AppLayout::compute(Rect::new(0, 0, 120, 40), 70, None, None);
        for panel_id in [
            PanelId::Projects,
            PanelId::Services,
            PanelId::Containers,
            PanelId::Images,
            PanelId::Volumes,
            PanelId::Networks,
            PanelId::Main,
            PanelId::Status,
        ] {
            assert!(layout.area(panel_id).is_some(), "{panel_id:?}");
        }
    }

    #[test]
    fn falls_back_to_main_only_when_terminal_is_tiny() {
        let area = Rect::new(0, 0, 10, 4);
        let layout = AppLayout::compute(area, 70, None, None);
        assert_eq!(layout.area(PanelId::Main), Some(area));
        assert_eq!(layout.area(PanelId::Projects), None);
    }

    #[test]
    fn compact_sidebar_shows_only_active_panel() {
        let layout = AppLayout::compute(
            Rect::new(0, 0, 60, 24),
            70,
            Some(PanelId::Containers),
            None,
        );
        assert!(layout.area(PanelId::Main).is_some());
        assert!(layout.area(PanelId::Status).is_some());
        assert!(layout.area(PanelId::Containers).is_some());
        assert!(layout.area(PanelId::Projects).is_none());
        assert!(layout.area(PanelId::Images).is_none());
    }

    #[test]
    fn compact_sidebar_can_switch_active_panel() {
        let layout = AppLayout::compute(
            Rect::new(0, 0, 60, 24),
            70,
            Some(PanelId::Images),
            None,
        );
        assert!(layout.area(PanelId::Images).is_some());
        assert!(layout.area(PanelId::Containers).is_none());
    }

    #[test]
    fn compact_sidebar_gives_sidebar_40_percent() {
        let layout = AppLayout::compute(
            Rect::new(0, 0, 60, 24),
            70,
            Some(PanelId::Containers),
            None,
        );
        let sidebar_width = layout.area(PanelId::Containers).unwrap().width;
        let main_width = layout.area(PanelId::Main).unwrap().width;
        // 40% of 60 = 24, but ratatui may round; just ensure sidebar is substantial
        assert!(sidebar_width >= 20, "sidebar should be ~24 cols, got {sidebar_width}");
        assert!(main_width > sidebar_width);
    }

    #[test]
    fn short_terminal_uses_single_line_status() {
        let layout = AppLayout::compute(Rect::new(0, 0, 120, 8), 70, None, None);
        let status = layout.area(PanelId::Status).unwrap();
        assert_eq!(status.height, 1);
    }

    #[test]
    fn recomputes_on_resize() {
        let wide = AppLayout::compute(Rect::new(0, 0, 120, 40), 70, None, None);
        let narrow = AppLayout::compute(Rect::new(0, 0, 80, 24), 70, None, None);
        assert_ne!(wide.area(PanelId::Main), narrow.area(PanelId::Main));
    }

    #[test]
    fn forced_mode_overrides_auto_detect() {
        let normal = AppLayout::compute(Rect::new(0, 0, 60, 24), 70, Some(PanelId::Containers), Some(LayoutMode::Normal));
        assert!(normal.area(PanelId::Projects).is_some());
        assert!(normal.area(PanelId::Containers).is_some());

        let compact = AppLayout::compute(Rect::new(0, 0, 120, 40), 70, Some(PanelId::Containers), Some(LayoutMode::CompactSidebar));
        assert!(compact.area(PanelId::Projects).is_none());
        assert!(compact.area(PanelId::Containers).is_some());

        let tiny = AppLayout::compute(Rect::new(0, 0, 120, 40), 70, None, Some(LayoutMode::Tiny));
        assert!(tiny.area(PanelId::Main).is_some());
        assert!(tiny.area(PanelId::Projects).is_none());
    }
}
