use super::*;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct ExportPreviewEditState {
    pub(super) label_overrides: Vec<String>,
    pub(super) label_positions: Vec<Option<[i32; 2]>>,
    pub(super) label_anchor_x: Vec<Option<f64>>,
    pub(super) preview_selections: Option<PlotSelections>,
    pub(super) text_annotations: Vec<ExportTextAnnotation>,
    pub(super) ink_strokes: Vec<ExportInkStroke>,
    pub(super) arrow_annotations: Vec<ExportArrowAnnotation>,
    pub(super) rectangle_annotations: Vec<ExportRectangleAnnotation>,
    pub(super) arrow_size: f32,
    pub(super) arrow_color_style: ExportArrowColorStyle,
    pub(super) style_preset: ExportStylePreset,
    pub(super) pane_scope: ExportPaneScope,
    pub(super) time_range_mode: ExportTimeRangeMode,
    pub(super) manual_start: f64,
    pub(super) manual_end: f64,
    pub(super) arrow_line_style: ExportArrowLineStyle,
    pub(super) manual_arrow_head_enabled: bool,
    pub(super) arrow_custom_color: Color32,
    pub(super) shape_kind: ShapeKind,
    pub(super) label_scale: i32,
    pub(super) label_font_style: ExportLabelFontStyle,
    pub(super) resolution: ExportResolution,
    pub(super) dpi: ExportDpi,
    pub(super) dpi_value: u32,
    pub(super) image_format: ExportImageFormat,
    pub(super) annotations_visible: bool,
    pub(super) auto_variable_labels_enabled: bool,
    pub(super) cursor_table_enabled: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(super) enum ExportLabelFontStyle {
    Regular,
    Bold,
    Outline,
}

impl ExportLabelFontStyle {
    pub(super) const ALL: [Self; 3] = [Self::Regular, Self::Bold, Self::Outline];

    pub(super) fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Regular, Language::Zh) => "常规",
            (Self::Bold, Language::Zh) => "加粗",
            (Self::Outline, Language::Zh) => "描边",
            (Self::Regular, Language::En) => "Regular",
            (Self::Bold, Language::En) => "Bold",
            (Self::Outline, Language::En) => "Outline",
        }
    }

    pub(super) fn text_style(self) -> TextStyle {
        match self {
            Self::Regular => TextStyle::Regular,
            Self::Bold => TextStyle::Bold,
            Self::Outline => TextStyle::Outline,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(super) enum ExportResolution {
    Standard,
    High,
    Ultra,
}

impl ExportResolution {
    pub(super) fn width(self) -> usize {
        match self {
            Self::Standard => 1600,
            Self::High => 2400,
            Self::Ultra => 3200,
        }
    }

    pub(super) fn scale(self) -> i32 {
        match self {
            Self::Standard => 1,
            Self::High => 2,
            Self::Ultra => 2,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(super) enum ExportDpi {
    Dpi150,
    Dpi300,
    Dpi600,
}

impl ExportDpi {
    pub(super) const ALL: [Self; 3] = [Self::Dpi150, Self::Dpi300, Self::Dpi600];

    pub(super) fn label(self, language: Language) -> &'static str {
        match (self, language) {
            (Self::Dpi150, Language::Zh | Language::En) => "150 DPI",
            (Self::Dpi300, Language::Zh | Language::En) => "300 DPI",
            (Self::Dpi600, Language::Zh | Language::En) => "600 DPI",
        }
    }

    pub(super) fn value(self) -> u32 {
        match self {
            Self::Dpi150 => 150,
            Self::Dpi300 => 300,
            Self::Dpi600 => 600,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub(super) enum ExportImageFormat {
    Png,
    Svg,
}

impl ExportImageFormat {
    pub(super) const ALL: [Self; 2] = [Self::Png, Self::Svg];

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Svg => "SVG",
        }
    }

    pub(super) fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Svg => "svg",
        }
    }
}

pub(super) enum RenderedWaveformImage {
    Png { canvas: Canvas, dpi: u32 },
    Svg(SvgCanvas),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ExportPreviewPrimaryAction {
    SaveImage,
    CopyImage,
}

impl RenderedWaveformImage {
    pub(super) fn save(&self, path: &Path) -> Result<(), String> {
        match self {
            Self::Png { canvas, dpi } => canvas
                .save_png_with_dpi(path, Some(*dpi))
                .map_err(|error| error.to_string()),
            Self::Svg(canvas) => canvas.save_svg(path).map_err(|error| error.to_string()),
        }
    }
}

pub(super) fn ensure_export_extension(path: &mut PathBuf, format: ExportImageFormat) {
    if path.extension().is_none() {
        path.set_extension(format.extension());
    }
}

impl ScopeApp {
    pub(super) fn export_preview_primary_actions() -> [ExportPreviewPrimaryAction; 2] {
        [
            ExportPreviewPrimaryAction::SaveImage,
            ExportPreviewPrimaryAction::CopyImage,
        ]
    }

    pub(super) fn clipboard_bitmap_bytes(size: [usize; 2], rgba: &[u8]) -> Result<Vec<u8>, String> {
        let [width, height] = size;
        let expected_len = width
            .checked_mul(height)
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "clipboard bitmap size is too large".to_owned())?;
        if width == 0 || height == 0 {
            return Err("clipboard bitmap size must be non-zero".to_owned());
        }
        if rgba.len() != expected_len {
            return Err(format!(
                "clipboard bitmap expected {expected_len} RGBA bytes, got {}",
                rgba.len()
            ));
        }

        let pixel_bytes = expected_len;
        let file_size = 54usize
            .checked_add(pixel_bytes)
            .ok_or_else(|| "clipboard bitmap file is too large".to_owned())?;
        let mut bytes = Vec::with_capacity(file_size);
        bytes.extend_from_slice(b"BM");
        bytes.extend_from_slice(&(file_size as u32).to_le_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&(54u32).to_le_bytes());
        bytes.extend_from_slice(&(40u32).to_le_bytes());
        bytes.extend_from_slice(&(width as i32).to_le_bytes());
        bytes.extend_from_slice(&(height as i32).to_le_bytes());
        bytes.extend_from_slice(&(1u16).to_le_bytes());
        bytes.extend_from_slice(&(32u16).to_le_bytes());
        bytes.extend_from_slice(&(0u32).to_le_bytes());
        bytes.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
        bytes.extend_from_slice(&(0i32).to_le_bytes());
        bytes.extend_from_slice(&(0i32).to_le_bytes());
        bytes.extend_from_slice(&(0u32).to_le_bytes());
        bytes.extend_from_slice(&(0u32).to_le_bytes());

        for y in (0..height).rev() {
            let row_start = y * width * 4;
            for pixel in rgba[row_start..row_start + width * 4].chunks_exact(4) {
                bytes.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]);
            }
        }
        Ok(bytes)
    }

    pub(super) fn copy_export_preview_image_to_clipboard(&mut self) {
        let selections = self.export_preview_plot_selections();
        match self
            .render_current_waveform_canvas(&selections)
            .and_then(|canvas| Self::copy_canvas_to_clipboard(&canvas))
        {
            Ok(()) => {
                self.last_error = Some(
                    self.tr("图片已复制到剪切板。", "Image copied to clipboard.")
                        .to_owned(),
                );
            }
            Err(error) => {
                let prefix = self.tr("复制图片失败", "Failed to copy image");
                self.last_error = Some(format!("{prefix}: {error}"));
            }
        }
    }

    fn copy_canvas_to_clipboard(canvas: &Canvas) -> Result<(), String> {
        let bitmap = Self::clipboard_bitmap_bytes(canvas.size(), canvas.pixels())?;
        Self::set_clipboard_bitmap(&bitmap)
    }

    #[cfg(windows)]
    fn set_clipboard_bitmap(bitmap: &[u8]) -> Result<(), String> {
        clipboard_win::set_clipboard(clipboard_win::formats::Bitmap, bitmap)
            .map_err(|error| error.to_string())
    }

    #[cfg(not(windows))]
    fn set_clipboard_bitmap(_bitmap: &[u8]) -> Result<(), String> {
        Err("image clipboard copy is only available on Windows".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_export_extension_only_adds_missing_extension() {
        let mut without_extension = std::path::PathBuf::from("waveform");
        ensure_export_extension(&mut without_extension, ExportImageFormat::Png);
        assert_eq!(without_extension, std::path::PathBuf::from("waveform.png"));

        let mut with_extension = std::path::PathBuf::from("waveform.custom");
        ensure_export_extension(&mut with_extension, ExportImageFormat::Svg);
        assert_eq!(with_extension, std::path::PathBuf::from("waveform.custom"));
    }

    #[test]
    fn export_label_font_style_uses_readable_chinese_labels() {
        assert_eq!(ExportLabelFontStyle::Regular.label(Language::Zh), "常规");
        assert_eq!(ExportLabelFontStyle::Bold.label(Language::Zh), "加粗");
        assert_eq!(ExportLabelFontStyle::Outline.label(Language::Zh), "描边");
    }

    #[test]
    fn rendered_png_saves_with_selected_dpi() {
        let path = std::env::temp_dir().join(format!(
            "scope_rendered_waveform_{}.png",
            std::process::id()
        ));
        let mut canvas = Canvas::new(4, 4, Rgba::rgb(255, 255, 255));
        canvas.line(0, 0, 3, 3, Rgba::rgb(12, 34, 56), 1);
        RenderedWaveformImage::Png { canvas, dpi: 300 }
            .save(&path)
            .unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"\x89PNG\r\n\x1A\n"));
        assert!(bytes.windows(4).any(|window| window == b"pHYs"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn clipboard_bitmap_bytes_convert_rgba_pixels_to_bgra() {
        let rgba = [10, 20, 30, 255, 40, 50, 60, 255];
        let bytes = ScopeApp::clipboard_bitmap_bytes([2, 1], &rgba).unwrap();

        assert_eq!(&bytes[0..2], b"BM");
        assert_eq!(u32::from_le_bytes(bytes[10..14].try_into().unwrap()), 54);
        assert_eq!(i32::from_le_bytes(bytes[18..22].try_into().unwrap()), 2);
        assert_eq!(i32::from_le_bytes(bytes[22..26].try_into().unwrap()), 1);
        assert_eq!(&bytes[54..58], &[30, 20, 10, 255]);
        assert_eq!(&bytes[58..62], &[60, 50, 40, 255]);
    }
}
