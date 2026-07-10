#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasPoint {
    pub x: i32,
    pub y: i32,
}

impl CanvasPoint {
    pub const fn new(x: i32, y: i32) -> Self {
        Self { x, y }
    }

    pub const fn to_array(self) -> [i32; 2] {
        [self.x, self.y]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

#[allow(dead_code)]
impl CanvasRect {
    pub const fn width(self) -> i32 {
        self.right - self.left
    }

    pub const fn height(self) -> i32 {
        self.bottom - self.top
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnnotationColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationLineStyle {
    Solid,
    Dashed,
    Dotted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShapeKind {
    Rectangle,
    Ellipse,
}

#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub struct VariableLabelAnnotation {
    pub label_index: usize,
    pub text: String,
    pub label_position: Option<CanvasPoint>,
    pub anchor_x: Option<f64>,
    pub visible: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextAnnotation {
    pub text: String,
    pub position: CanvasPoint,
    pub color: AnnotationColor,
    pub scale: i32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ArrowAnnotation {
    pub start: CanvasPoint,
    pub end: CanvasPoint,
    pub color: AnnotationColor,
    pub width: i32,
    pub head_size: f32,
    pub line_style: AnnotationLineStyle,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RectangleAnnotation {
    pub kind: ShapeKind,
    pub rect: CanvasRect,
    pub color: AnnotationColor,
    pub width: i32,
    pub line_style: AnnotationLineStyle,
}

impl RectangleAnnotation {
    #[allow(dead_code)]
    pub fn from_corners(
        start: CanvasPoint,
        end: CanvasPoint,
        color: AnnotationColor,
        width: i32,
        line_style: AnnotationLineStyle,
    ) -> Self {
        Self::from_corners_with_kind(start, end, ShapeKind::Rectangle, color, width, line_style)
    }

    pub fn from_corners_with_kind(
        start: CanvasPoint,
        end: CanvasPoint,
        kind: ShapeKind,
        color: AnnotationColor,
        width: i32,
        line_style: AnnotationLineStyle,
    ) -> Self {
        Self {
            kind,
            rect: CanvasRect {
                left: start.x.min(end.x),
                top: start.y.min(end.y),
                right: start.x.max(end.x),
                bottom: start.y.max(end.y),
            },
            color,
            width: width.max(1),
            line_style,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct InkStroke {
    pub points: Vec<CanvasPoint>,
    pub color: AnnotationColor,
    pub width: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
#[allow(dead_code)]
pub struct AnnotationDocument {
    pub variable_labels: Vec<VariableLabelAnnotation>,
    pub text_annotations: Vec<TextAnnotation>,
    pub arrow_annotations: Vec<ArrowAnnotation>,
    pub rectangle_annotations: Vec<RectangleAnnotation>,
    pub ink_strokes: Vec<InkStroke>,
}

pub fn clamp_label_position(
    position: CanvasPoint,
    bounds: CanvasRect,
    text_width: i32,
    text_height: i32,
) -> CanvasPoint {
    let min_x = bounds.left + 5;
    let max_x = (bounds.right - text_width - 5).max(min_x);
    let min_y = bounds.top + 5;
    let max_y = (bounds.bottom - text_height - 5).max(min_y);
    CanvasPoint::new(
        position.x.clamp(min_x, max_x),
        position.y.clamp(min_y, max_y),
    )
}

#[allow(dead_code)]
pub fn arrow_start_for_label(label: CanvasRect, target: CanvasPoint) -> CanvasPoint {
    let center_y = label.top + label.height() / 2;
    if target.x < label.left {
        CanvasPoint::new(label.left - 5, center_y)
    } else {
        CanvasPoint::new(label.right + 5, center_y)
    }
}

pub fn point_segment_distance_sq(point: CanvasPoint, start: CanvasPoint, end: CanvasPoint) -> f64 {
    let vx = (end.x - start.x) as f64;
    let vy = (end.y - start.y) as f64;
    let wx = (point.x - start.x) as f64;
    let wy = (point.y - start.y) as f64;
    let len_sq = vx * vx + vy * vy;
    if len_sq <= f64::EPSILON {
        return ((point.x - start.x) as f64).powi(2) + ((point.y - start.y) as f64).powi(2);
    }
    let t = ((wx * vx + wy * vy) / len_sq).clamp(0.0, 1.0);
    let cx = start.x as f64 + vx * t;
    let cy = start.y as f64 + vy * t;
    (point.x as f64 - cx).powi(2) + (point.y as f64 - cy).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_position_clamps_to_canvas_bounds() {
        let bounds = CanvasRect {
            left: 0,
            top: 0,
            right: 1200,
            bottom: 900,
        };

        assert_eq!(
            clamp_label_position(CanvasPoint::new(1170, 890), bounds, 160, 32),
            CanvasPoint::new(1035, 863)
        );
        assert_eq!(
            clamp_label_position(CanvasPoint::new(48, 720), bounds, 160, 32),
            CanvasPoint::new(48, 720)
        );
    }

    #[test]
    fn arrow_start_follows_label_side_nearest_target() {
        let label = CanvasRect {
            left: 100,
            top: 40,
            right: 180,
            bottom: 70,
        };

        assert_eq!(
            arrow_start_for_label(label, CanvasPoint::new(60, 55)),
            CanvasPoint::new(95, 55)
        );
        assert_eq!(
            arrow_start_for_label(label, CanvasPoint::new(260, 55)),
            CanvasPoint::new(185, 55)
        );
    }

    #[test]
    fn point_segment_distance_handles_degenerate_segments() {
        let point = CanvasPoint::new(13, 14);
        let start = CanvasPoint::new(10, 10);
        assert_eq!(point_segment_distance_sq(point, start, start), 25.0);
    }

    #[test]
    fn rectangle_annotation_normalizes_drag_corners() {
        let rect = RectangleAnnotation::from_corners(
            CanvasPoint::new(320, 240),
            CanvasPoint::new(120, 90),
            AnnotationColor {
                r: 220,
                g: 20,
                b: 38,
                a: 255,
            },
            4,
            AnnotationLineStyle::Solid,
        );

        assert_eq!(
            rect.rect,
            CanvasRect {
                left: 120,
                top: 90,
                right: 320,
                bottom: 240
            }
        );
        assert_eq!(rect.kind, ShapeKind::Rectangle);
        assert_eq!(rect.width, 4);

        let ellipse = RectangleAnnotation::from_corners_with_kind(
            CanvasPoint::new(1, 2),
            CanvasPoint::new(11, 22),
            ShapeKind::Ellipse,
            rect.color,
            2,
            AnnotationLineStyle::Solid,
        );
        assert_eq!(ellipse.kind, ShapeKind::Ellipse);
        assert_eq!(ellipse.rect.width(), 10);
    }
}
