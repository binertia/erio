use ratatui::{Frame, layout::Rect};

use crate::{state::AppState, terminal::AppTerminal};

use super::{AppUi, layout::AppLayout, panel::RenderContext};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderScheduler {
    dirty: bool,
    last_size: Option<Rect>,
    render_count: u64,
}

impl Default for RenderScheduler {
    fn default() -> Self {
        Self {
            dirty: true,
            last_size: None,
            render_count: 0,
        }
    }
}

impl RenderScheduler {
    pub fn request_redraw(&mut self) {
        self.dirty = true;
    }

    pub fn mark_resize(&mut self) {
        self.dirty = true;
        self.last_size = None;
    }

    pub fn should_render(&self) -> bool {
        self.dirty
    }

    pub fn render_count(&self) -> u64 {
        self.render_count
    }

    fn finish_render(&mut self, size: Rect) {
        self.dirty = false;
        self.last_size = Some(size);
        self.render_count = self.render_count.saturating_add(1);
    }

    #[cfg(test)]
    pub fn finish_render_for_test(&mut self, size: Rect) {
        self.finish_render(size);
    }
}

pub fn draw(terminal: &mut AppTerminal, ui: &mut AppUi, state: &AppState) -> std::io::Result<()> {
    if !ui.render.should_render() {
        return Ok(());
    }

    let mut rendered_size = Rect::default();
    terminal.draw(|frame| {
        rendered_size = frame.area();
        render_frame(frame, ui, state);
    })?;
    ui.render.finish_render(rendered_size);

    Ok(())
}

fn render_frame(frame: &mut Frame<'_>, ui: &AppUi, state: &AppState) {
    let layout = AppLayout::compute(frame.area());
    let focused = ui.focus.focused();

    for panel in ui.panels.iter() {
        let area = layout.area(panel.id());
        let Some(area) = area else {
            continue;
        };

        let context = RenderContext {
            state,
            focused: panel.id() == focused,
        };
        panel.render(frame, area, context);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_coalesces_rapid_redraw_requests() {
        let mut scheduler = RenderScheduler::default();
        assert!(scheduler.should_render());
        scheduler.request_redraw();
        scheduler.request_redraw();
        scheduler.finish_render(Rect::new(0, 0, 80, 24));
        assert!(!scheduler.should_render());
        assert_eq!(scheduler.render_count(), 1);
    }

    #[test]
    fn resize_forces_redraw() {
        let mut scheduler = RenderScheduler::default();
        scheduler.finish_render(Rect::new(0, 0, 80, 24));
        scheduler.mark_resize();
        assert!(scheduler.should_render());
    }
}
