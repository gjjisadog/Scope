#![allow(clippy::too_many_arguments)]

use std::{fs, path::Path};

use crate::png_export::{ClipRect, Rgba, StrokeStyle, TextStyle, WaveformCanvas};

pub struct SvgCanvas {
    width: usize,
    height: usize,
    background: Rgba,
    elements: Vec<String>,
}

impl SvgCanvas {
    pub fn new(width: usize, height: usize, background: Rgba) -> Self {
        Self {
            width,
            height,
            background,
            elements: Vec::new(),
        }
    }

    pub fn save_svg(&self, path: &Path) -> std::io::Result<()> {
        let mut svg =
            String::with_capacity(self.elements.iter().map(String::len).sum::<usize>() + 512);
        svg.push_str(r#"<?xml version="1.0" encoding="UTF-8"?>"#);
        svg.push('\n');
        svg.push_str(&format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{}" height="{}" viewBox="0 0 {} {}">"#,
            self.width, self.height, self.width, self.height
        ));
        svg.push('\n');
        svg.push_str(&format!(
            r#"<rect x="0" y="0" width="{}" height="{}" fill="{}"{} />"#,
            self.width,
            self.height,
            color_hex(self.background),
            opacity_attr("fill", self.background)
        ));
        svg.push('\n');
        for element in &self.elements {
            svg.push_str(element);
            svg.push('\n');
        }
        svg.push_str("</svg>\n");
        fs::write(path, svg)
    }

    fn line_styled(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Rgba,
        width: i32,
        style: StrokeStyle,
    ) {
        let dash = match style {
            StrokeStyle::Solid => "",
            StrokeStyle::Dashed => r#" stroke-dasharray="16 9""#,
            StrokeStyle::Dotted => r#" stroke-dasharray="2 8""#,
        };
        self.elements.push(format!(
            r#"<line x1="{x0}" y1="{y0}" x2="{x1}" y2="{y1}" stroke="{}"{} stroke-width="{}" stroke-linecap="round"{} fill="none" />"#,
            color_hex(color),
            opacity_attr("stroke", color),
            width.max(1),
            dash
        ));
    }
}

impl WaveformCanvas for SvgCanvas {
    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        if right <= left || bottom <= top {
            return;
        }
        self.elements.push(format!(
            r#"<rect x="{left}" y="{top}" width="{}" height="{}" fill="{}"{} />"#,
            right - left,
            bottom - top,
            color_hex(color),
            opacity_attr("fill", color)
        ));
    }

    fn stroke_rect(
        &mut self,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        color: Rgba,
        width: i32,
    ) {
        if right <= left || bottom <= top {
            return;
        }
        let stroke = width.max(1);
        self.elements.push(format!(
            r#"<rect x="{left}" y="{top}" width="{}" height="{}" fill="none" stroke="{}"{} stroke-width="{stroke}" />"#,
            right - left,
            bottom - top,
            color_hex(color),
            opacity_attr("stroke", color)
        ));
    }

    fn stroke_ellipse(
        &mut self,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        color: Rgba,
        width: i32,
    ) {
        if right <= left || bottom <= top {
            return;
        }
        let cx = (left + right) as f32 / 2.0;
        let cy = (top + bottom) as f32 / 2.0;
        let rx = (right - left) as f32 / 2.0;
        let ry = (bottom - top) as f32 / 2.0;
        let stroke = width.max(1);
        self.elements.push(format!(
            r#"<ellipse cx="{cx:.1}" cy="{cy:.1}" rx="{rx:.1}" ry="{ry:.1}" fill="none" stroke="{}"{} stroke-width="{stroke}" />"#,
            color_hex(color),
            opacity_attr("stroke", color)
        ));
    }

    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgba, width: i32) {
        self.line_styled(x0, y0, x1, y1, color, width, StrokeStyle::Solid);
    }

    fn line_styled(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Rgba,
        width: i32,
        style: StrokeStyle,
    ) {
        SvgCanvas::line_styled(self, x0, y0, x1, y1, color, width, style);
    }

    fn line_clipped(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Rgba,
        width: i32,
        clip: ClipRect,
    ) {
        if let Some((x0, y0, x1, y1)) = clip_line(x0, y0, x1, y1, clip) {
            self.line(x0, y0, x1, y1, color, width);
        }
    }

    fn arrow(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Rgba,
        head_size: f32,
        width: i32,
        style: StrokeStyle,
    ) {
        let width = width.max(1);
        self.line_styled(x0, y0, x1, y1, color, width, style);
        if head_size <= 0.0 {
            return;
        }
        let angle = ((y1 - y0) as f32).atan2((x1 - x0) as f32);
        let head = head_size.max(3.0);
        for offset in [2.55_f32, -2.55_f32] {
            let a = angle + offset;
            let hx = x1 as f32 + a.cos() * head;
            let hy = y1 as f32 + a.sin() * head;
            self.line(x1, y1, hx.round() as i32, hy.round() as i32, color, width);
        }
    }

    fn text(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        self.text_styled(x, y, text, color, scale, TextStyle::Regular);
    }

    fn text_styled(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: Rgba,
        scale: i32,
        style: TextStyle,
    ) {
        let scale = scale.max(1);
        let font_size = (8.8 * scale as f32).max(8.0);
        let baseline = y as f32 + font_size;
        let weight = match style {
            TextStyle::Bold => r#" font-weight="700""#,
            TextStyle::Regular | TextStyle::Outline => "",
        };
        let outline = match style {
            TextStyle::Outline => {
                r##" stroke="#ffffff" stroke-width="2" stroke-linejoin="round" paint-order="stroke fill""##
            }
            TextStyle::Regular | TextStyle::Bold => "",
        };
        self.elements.push(format!(
            r#"<text x="{x}" y="{baseline:.2}" font-family="Microsoft YaHei, Segoe UI, Arial, sans-serif" font-size="{font_size:.2}"{} fill="{}"{}{}>{}</text>"#,
            weight,
            color_hex(color),
            opacity_attr("fill", color),
            outline,
            escape_xml(text)
        ));
    }
}

fn color_hex(color: Rgba) -> String {
    format!("#{:02x}{:02x}{:02x}", color.r, color.g, color.b)
}

fn opacity_attr(kind: &str, color: Rgba) -> String {
    if color.a == 255 {
        String::new()
    } else {
        format!(r#" {kind}-opacity="{:.3}""#, color.a as f32 / 255.0)
    }
}

fn escape_xml(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn clip_line(
    mut x0: i32,
    mut y0: i32,
    mut x1: i32,
    mut y1: i32,
    clip: ClipRect,
) -> Option<(i32, i32, i32, i32)> {
    let mut code0 = out_code(x0, y0, clip);
    let mut code1 = out_code(x1, y1, clip);
    loop {
        if code0 | code1 == 0 {
            return Some((x0, y0, x1, y1));
        }
        if code0 & code1 != 0 {
            return None;
        }
        let code = if code0 != 0 { code0 } else { code1 };
        let mut x = 0;
        let mut y = 0;
        if code & 8 != 0 {
            if y1 == y0 {
                return None;
            }
            x = x0 + (x1 - x0) * (clip.bottom - y0) / (y1 - y0);
            y = clip.bottom;
        } else if code & 4 != 0 {
            if y1 == y0 {
                return None;
            }
            x = x0 + (x1 - x0) * (clip.top - y0) / (y1 - y0);
            y = clip.top;
        } else if code & 2 != 0 {
            if x1 == x0 {
                return None;
            }
            y = y0 + (y1 - y0) * (clip.right - x0) / (x1 - x0);
            x = clip.right;
        } else if code & 1 != 0 {
            if x1 == x0 {
                return None;
            }
            y = y0 + (y1 - y0) * (clip.left - x0) / (x1 - x0);
            x = clip.left;
        }
        if code == code0 {
            x0 = x;
            y0 = y;
            code0 = out_code(x0, y0, clip);
        } else {
            x1 = x;
            y1 = y;
            code1 = out_code(x1, y1, clip);
        }
    }
}

fn out_code(x: i32, y: i32, clip: ClipRect) -> u8 {
    let mut code = 0;
    if x < clip.left {
        code |= 1;
    } else if x > clip.right {
        code |= 2;
    }
    if y < clip.top {
        code |= 4;
    } else if y > clip.bottom {
        code |= 8;
    }
    code
}
