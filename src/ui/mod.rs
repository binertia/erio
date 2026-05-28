pub mod focus;
pub mod input;
pub mod layout;
pub mod panel;
pub mod renderer;

use focus::FocusManager;
use layout::AppLayout;
use panel::{PanelId, PanelRegistry, build_core_panels};
use ratatui::layout::Rect;
use renderer::RenderScheduler;

#[derive(Debug)]
pub struct AppUi {
    pub focus: FocusManager,
    pub panels: PanelRegistry,
    pub render: RenderScheduler,
    cached_layout: Option<(Rect, AppLayout)>,
}

impl Default for AppUi {
    fn default() -> Self {
        let panels = build_core_panels();
        let focus = FocusManager::new(panels.focusable_ids());

        Self {
            focus,
            panels,
            render: RenderScheduler::default(),
            cached_layout: None,
        }
    }
}

impl AppUi {
    /// Returns the cached layout if the terminal area hasn't changed,
    /// otherwise recomputes and caches the new layout.
    pub fn layout(
        &mut self,
        area: Rect,
        compact_threshold: u16,
        active_sidebar: Option<PanelId>,
        forced_mode: Option<crate::ui::layout::LayoutMode>,
    ) -> AppLayout {
        if let Some((cached_area, cached_layout)) = self.cached_layout
            && cached_area == area
        {
            return cached_layout;
        }
        let layout = AppLayout::compute(area, compact_threshold, active_sidebar, forced_mode);
        self.cached_layout = Some((area, layout));
        layout
    }

    /// Invalidates the cached layout, forcing recomputation on next render.
    pub fn invalidate_layout(&mut self) {
        self.cached_layout = None;
    }

    /// Returns the panel ID at the given (column, row) coordinate,
    /// using the cached layout if available.
    pub fn panel_at(&self, column: u16, row: u16) -> Option<PanelId> {
        let (_, layout) = self.cached_layout?;
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
            if let Some(rect) = layout.area(panel_id)
                && rect.x <= column
                && column < rect.x + rect.width
                && rect.y <= row
                && row < rect.y + rect.height
            {
                return Some(panel_id);
            }
        }
        None
    }
}
