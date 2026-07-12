#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CursorId {
    #[default]
    A,
    B,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlotViewport {
    pub initialized: bool,
    pub view_start: f64,
    pub view_end: f64,
    pub y_min: Option<f64>,
    pub y_max: Option<f64>,
    pub pane_y_bounds: Vec<Option<(f64, f64)>>,
    pub active_scope_pane: usize,
    pub cursor_a: f64,
    pub cursor_b: f64,
    pub show_cursor_a: bool,
    pub show_cursor_b: bool,
    pub active_cursor: CursorId,
}

impl Default for PlotViewport {
    fn default() -> Self {
        Self {
            initialized: false,
            view_start: 0.0,
            view_end: 1.0,
            y_min: None,
            y_max: None,
            pane_y_bounds: vec![None],
            active_scope_pane: 0,
            cursor_a: 0.25,
            cursor_b: 0.75,
            show_cursor_a: true,
            show_cursor_b: true,
            active_cursor: CursorId::A,
        }
    }
}

impl PlotViewport {
    pub fn reset_to_range(&mut self, start: f64, end: f64) {
        let (start, end) = normalized_range(start, end);
        self.view_start = start;
        self.view_end = end;
        self.y_min = None;
        self.y_max = None;
        self.cursor_a = start + (end - start) * 0.33;
        self.cursor_b = start + (end - start) * 0.66;
        self.show_cursor_a = true;
        self.show_cursor_b = true;
        self.active_cursor = CursorId::A;
        self.active_scope_pane = 0;
        self.pane_y_bounds.fill(None);
        self.initialized = true;
    }

    pub fn cursor_range(&self) -> (f64, f64) {
        if self.cursor_a <= self.cursor_b {
            (self.cursor_a, self.cursor_b)
        } else {
            (self.cursor_b, self.cursor_a)
        }
    }

    pub fn set_pane_count(&mut self, pane_count: usize) {
        let pane_count = pane_count.max(1);
        self.pane_y_bounds.resize(pane_count, None);
        self.pane_y_bounds.truncate(pane_count);
        self.active_scope_pane = self.active_scope_pane.min(pane_count - 1);
    }

    pub fn pan_x(&mut self, delta: f64) {
        if delta.is_finite() {
            self.view_start += delta;
            self.view_end += delta;
            if self.view_start > self.view_end {
                std::mem::swap(&mut self.view_start, &mut self.view_end);
            }
            self.initialized = true;
        }
    }

    pub fn zoom_x(&mut self, anchor: f64, factor: f64) {
        if !anchor.is_finite() || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let (start, end) = normalized_range(self.view_start, self.view_end);
        self.view_start = anchor + (start - anchor) * factor;
        self.view_end = anchor + (end - anchor) * factor;
        if (self.view_end - self.view_start).abs() < f64::EPSILON {
            self.view_end = self.view_start + f64::EPSILON;
        }
        self.initialized = true;
    }
}

fn normalized_range(start: f64, end: f64) -> (f64, f64) {
    let start = if start.is_finite() { start } else { 0.0 };
    let end = if end.is_finite() { end } else { start + 1.0 };
    if end > start {
        (start, end)
    } else {
        (end, start.max(end + f64::EPSILON))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewport_shares_cursor_zoom_pan_and_pane_invariants() {
        let mut viewport = PlotViewport::default();
        viewport.reset_to_range(10.0, 20.0);
        assert_eq!(viewport.cursor_range(), (13.3, 16.6));
        viewport.zoom_x(15.0, 0.5);
        assert_eq!((viewport.view_start, viewport.view_end), (12.5, 17.5));
        viewport.pan_x(2.0);
        assert_eq!((viewport.view_start, viewport.view_end), (14.5, 19.5));
        viewport.set_pane_count(3);
        viewport.active_scope_pane = 2;
        viewport.set_pane_count(1);
        assert_eq!(viewport.active_scope_pane, 0);
    }
}
