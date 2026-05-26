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

impl AppLayout {
    pub fn compute(area: Rect) -> Self {
        if area.width < 24 || area.height < 8 {
            return Self {
                projects: None,
                services: None,
                containers: None,
                images: None,
                volumes: None,
                networks: None,
                main: area,
                status: None,
            };
        }

        let vertical = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(3), Constraint::Length(3)])
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
        let layout = AppLayout::compute(Rect::new(0, 0, 120, 40));
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
        let layout = AppLayout::compute(area);
        assert_eq!(layout.area(PanelId::Main), Some(area));
        assert_eq!(layout.area(PanelId::Projects), None);
    }

    #[test]
    fn recomputes_on_resize() {
        let wide = AppLayout::compute(Rect::new(0, 0, 120, 40));
        let narrow = AppLayout::compute(Rect::new(0, 0, 80, 24));
        assert_ne!(wide.area(PanelId::Main), narrow.area(PanelId::Main));
    }
}
