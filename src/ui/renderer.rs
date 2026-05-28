use std::time::{Duration, Instant};

use ratatui::{Frame, layout::Rect};

use crate::{state::AppState, terminal::AppTerminal};

use super::{AppUi, layout::AppLayout, panel::RenderContext};

#[derive(Debug, Clone)]
pub struct RenderScheduler {
    dirty: bool,
    last_size: Option<Rect>,
    render_count: u64,
    last_render_at: Option<Instant>,
    min_render_interval: Duration,
}

impl Default for RenderScheduler {
    fn default() -> Self {
        Self {
            dirty: true,
            last_size: None,
            render_count: 0,
            last_render_at: None,
            min_render_interval: Duration::from_millis(16),
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
        if !self.dirty {
            return false;
        }
        match self.last_render_at {
            None => true,
            Some(last) => last.elapsed() >= self.min_render_interval,
        }
    }

    pub fn render_count(&self) -> u64 {
        self.render_count
    }

    fn finish_render(&mut self, size: Rect) {
        self.dirty = false;
        self.last_size = Some(size);
        self.render_count = self.render_count.saturating_add(1);
        self.last_render_at = Some(Instant::now());
    }

    #[cfg(test)]
    pub fn finish_render_for_test(&mut self, size: Rect) {
        self.finish_render(size);
        // Clear timing so tests can immediately request another render
        self.last_render_at = None;
    }
}

pub fn draw(terminal: &mut AppTerminal, ui: &mut AppUi, state: &AppState) -> std::io::Result<()> {
    if !ui.render.should_render() {
        return Ok(());
    }

    let mut rendered_size = Rect::default();
    terminal.draw(|frame| {
        rendered_size = frame.area();
        let layout = ui.layout(
            frame.area(),
            state.compact_mode_width,
            Some(state.compact_sidebar_panel),
            state.screen_mode,
        );
        render_frame(frame, ui, state, layout);
    })?;
    ui.render.finish_render(rendered_size);

    Ok(())
}

fn render_frame(frame: &mut Frame<'_>, ui: &AppUi, state: &AppState, layout: AppLayout) {
    let focused = ui.focus.focused();

    for panel in ui.panels.iter() {
        let area = layout.area(panel.id());
        let Some(area) = area else {
            continue;
        };

        // Skip panels that are too small to render meaningfully
        if area.width < 2 || area.height < 2 {
            continue;
        }

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
        scheduler.finish_render_for_test(Rect::new(0, 0, 80, 24));
        scheduler.mark_resize();
        assert!(scheduler.should_render());
    }

    #[test]
    fn coalesces_rapid_resizes_with_time_gate() {
        let mut scheduler = RenderScheduler::default();
        scheduler.finish_render_for_test(Rect::new(0, 0, 80, 24));
        // Simulate a resize storm: mark dirty immediately after render
        scheduler.mark_resize();
        // With the 16ms gate, should_render returns true because we cleared timing in test helper
        assert!(scheduler.should_render());

        // After rendering again and marking resize without clearing timing,
        // the gate would block. In production this coalesces tmux resize storms.
        scheduler.finish_render(Rect::new(0, 0, 80, 24));
        scheduler.mark_resize();
        // Should be blocked because only nanoseconds have passed
        assert!(!scheduler.should_render());
    }
}
