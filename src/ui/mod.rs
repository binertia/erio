pub mod focus;
pub mod input;
pub mod layout;
pub mod panel;
pub mod renderer;

use focus::FocusManager;
use panel::{PanelRegistry, build_core_panels};
use renderer::RenderScheduler;

#[derive(Debug)]
pub struct AppUi {
    pub focus: FocusManager,
    pub panels: PanelRegistry,
    pub render: RenderScheduler,
}

impl Default for AppUi {
    fn default() -> Self {
        let panels = build_core_panels();
        let focus = FocusManager::new(panels.focusable_ids());

        Self {
            focus,
            panels,
            render: RenderScheduler::default(),
        }
    }
}
