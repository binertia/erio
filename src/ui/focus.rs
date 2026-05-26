use super::panel::PanelId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusManager {
    order: Vec<PanelId>,
    focused: PanelId,
}

impl FocusManager {
    pub fn new(order: Vec<PanelId>) -> Self {
        let focused = order
            .first()
            .copied()
            .unwrap_or(super::panel::PanelId::Main);
        Self { order, focused }
    }

    pub fn focused(&self) -> PanelId {
        self.focused
    }

    pub fn set_focus(&mut self, panel_id: PanelId) -> bool {
        if !self.order.contains(&panel_id) || self.focused == panel_id {
            return false;
        }

        self.focused = panel_id;
        true
    }

    pub fn focus_next(&mut self) -> bool {
        self.move_by(1)
    }

    pub fn focus_previous(&mut self) -> bool {
        self.move_by(-1)
    }

    fn move_by(&mut self, delta: isize) -> bool {
        if self.order.is_empty() {
            return false;
        }

        let current = self
            .order
            .iter()
            .position(|panel_id| *panel_id == self.focused)
            .unwrap_or(0);
        let len = self.order.len() as isize;
        let next = (current as isize + delta).rem_euclid(len) as usize;
        self.focused = self.order[next];
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycles_focus_in_order() {
        let mut focus = FocusManager::new(vec![PanelId::Projects, PanelId::Containers]);
        assert_eq!(focus.focused(), PanelId::Projects);
        assert!(focus.focus_next());
        assert_eq!(focus.focused(), PanelId::Containers);
        assert!(focus.focus_next());
        assert_eq!(focus.focused(), PanelId::Projects);
        assert!(focus.focus_previous());
        assert_eq!(focus.focused(), PanelId::Containers);
    }

    #[test]
    fn rejects_non_focusable_panel() {
        let mut focus = FocusManager::new(vec![PanelId::Projects]);
        assert!(!focus.set_focus(PanelId::Status));
        assert_eq!(focus.focused(), PanelId::Projects);
    }
}
