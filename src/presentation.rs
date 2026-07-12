use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ChannelPresentation {
    pub display_name: String,
    pub color: [u8; 4],
    pub visible: bool,
    pub scale: f32,
    pub pane: usize,
}

impl ChannelPresentation {
    pub fn new(display_name: impl Into<String>, color: [u8; 4]) -> Self {
        Self {
            display_name: display_name.into(),
            color,
            visible: true,
            scale: 1.0,
            pane: 0,
        }
    }

    pub fn sanitize(&mut self) {
        if !self.scale.is_finite() {
            self.scale = 1.0;
        }
        self.scale = self.scale.clamp(-1_000_000.0, 1_000_000.0);
    }
}

#[cfg(test)]
mod tests {
    use super::ChannelPresentation;

    #[test]
    fn presentation_sanitizes_non_finite_scale_without_losing_other_state() {
        let mut presentation = ChannelPresentation {
            display_name: "Ia".to_owned(),
            color: [1, 2, 3, 255],
            visible: false,
            scale: f32::NAN,
            pane: 2,
        };
        presentation.sanitize();
        assert_eq!(presentation.scale, 1.0);
        assert_eq!(presentation.pane, 2);
        assert!(!presentation.visible);
    }
}
