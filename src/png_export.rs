use ab_glyph::{point, Font, FontArc, PxScale, ScaleFont};
use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::Path,
    sync::OnceLock,
};

#[derive(Clone, Copy)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ClipRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

#[derive(Clone, Copy)]
pub enum TextStyle {
    Regular,
    Bold,
    Outline,
}

#[derive(Clone, Copy)]
pub enum StrokeStyle {
    Solid,
    Dashed,
    Dotted,
}

pub trait WaveformCanvas {
    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba);
    fn stroke_rect(
        &mut self,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        color: Rgba,
        width: i32,
    );
    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgba, width: i32);
    fn line_clipped(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Rgba,
        width: i32,
        clip: ClipRect,
    );
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
    );
    fn text(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32);
    fn text_styled(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: Rgba,
        scale: i32,
        style: TextStyle,
    );
}

impl Canvas {
    pub fn new(width: usize, height: usize, background: Rgba) -> Self {
        let mut pixels = vec![0; width.saturating_mul(height).saturating_mul(4)];
        for px in pixels.chunks_exact_mut(4) {
            px.copy_from_slice(&[background.r, background.g, background.b, background.a]);
        }
        Self {
            width,
            height,
            pixels,
        }
    }

    pub fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        let left = left.clamp(0, self.width as i32);
        let right = right.clamp(0, self.width as i32);
        let top = top.clamp(0, self.height as i32);
        let bottom = bottom.clamp(0, self.height as i32);
        if right <= left || bottom <= top {
            return;
        }
        for y in top..bottom {
            for x in left..right {
                self.set_pixel(x, y, color);
            }
        }
    }

    pub fn size(&self) -> [usize; 2] {
        [self.width, self.height]
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn stroke_rect(
        &mut self,
        left: i32,
        top: i32,
        right: i32,
        bottom: i32,
        color: Rgba,
        width: i32,
    ) {
        let width = width.max(1);
        self.fill_rect(left, top, right, top + width, color);
        self.fill_rect(left, bottom - width, right, bottom, color);
        self.fill_rect(left, top, left + width, bottom, color);
        self.fill_rect(right - width, top, right, bottom, color);
    }

    pub fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgba, width: i32) {
        let mut x0 = x0;
        let mut y0 = y0;
        let dx = (x1 - x0).abs();
        let sx = if x0 < x1 { 1 } else { -1 };
        let dy = -(y1 - y0).abs();
        let sy = if y0 < y1 { 1 } else { -1 };
        let mut err = dx + dy;
        loop {
            self.brush(x0, y0, color, width);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    pub fn line_clipped(
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

    pub fn arrow(
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
        let angle = ((y1 - y0) as f32).atan2((x1 - x0) as f32);
        let head = head_size.max(3.0);
        for offset in [2.55_f32, -2.55_f32] {
            let a = angle + offset;
            let hx = x1 as f32 + a.cos() * head;
            let hy = y1 as f32 + a.sin() * head;
            self.line(x1, y1, hx.round() as i32, hy.round() as i32, color, width);
        }
    }

    pub fn line_styled(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Rgba,
        width: i32,
        style: StrokeStyle,
    ) {
        match style {
            StrokeStyle::Solid => self.line(x0, y0, x1, y1, color, width),
            StrokeStyle::Dashed => self.segmented_line(x0, y0, x1, y1, color, width, 16.0, 9.0),
            StrokeStyle::Dotted => self.segmented_line(x0, y0, x1, y1, color, width, 2.0, 8.0),
        }
    }

    fn segmented_line(
        &mut self,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        color: Rgba,
        width: i32,
        on_len: f32,
        off_len: f32,
    ) {
        let dx = (x1 - x0) as f32;
        let dy = (y1 - y0) as f32;
        let len = (dx * dx + dy * dy).sqrt();
        if len <= 0.5 {
            self.line(x0, y0, x1, y1, color, width);
            return;
        }
        let ux = dx / len;
        let uy = dy / len;
        let mut start = 0.0_f32;
        while start < len {
            let end = (start + on_len).min(len);
            let sx = x0 as f32 + ux * start;
            let sy = y0 as f32 + uy * start;
            let ex = x0 as f32 + ux * end;
            let ey = y0 as f32 + uy * end;
            self.line(
                sx.round() as i32,
                sy.round() as i32,
                ex.round() as i32,
                ey.round() as i32,
                color,
                width,
            );
            start += on_len + off_len;
        }
    }

    pub fn text(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        self.text_styled(x, y, text, color, scale, TextStyle::Regular);
    }

    pub fn text_styled(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: Rgba,
        scale: i32,
        style: TextStyle,
    ) {
        let scale = scale.max(1);
        if self.text_ttf(x, y, text, color, scale, style) {
            return;
        }
        match style {
            TextStyle::Regular => self.text_raw(x, y, text, color, scale),
            TextStyle::Bold => {
                self.text_raw(x, y, text, color, scale);
                self.text_raw(x + (scale / 2).max(1), y, text, color, scale);
            }
            TextStyle::Outline => {
                let outline = Rgba::rgba(255, 255, 255, 235);
                for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                    self.text_raw(x + dx * scale, y + dy * scale, text, outline, scale);
                }
                self.text_raw(x, y, text, color, scale);
            }
        }
    }

    pub fn text_width(text: &str, scale: i32) -> i32 {
        let scale = scale.max(1);
        if let Some(width) = ttf_text_width(text, scale) {
            return width;
        }
        text.chars().count() as i32 * 6 * scale
    }

    pub fn text_height(scale: i32) -> i32 {
        let scale = scale.max(1);
        if let Some(font) = system_font() {
            let scaled = font.as_scaled(text_px_scale(scale));
            return (scaled.height().ceil() as i32).max(7 * scale);
        }
        7 * scale
    }

    fn text_ttf(
        &mut self,
        x: i32,
        y: i32,
        text: &str,
        color: Rgba,
        scale: i32,
        style: TextStyle,
    ) -> bool {
        let Some(font) = system_font() else {
            return false;
        };
        let outline = Rgba::rgba(255, 255, 255, 235);
        match style {
            TextStyle::Regular => self.text_ttf_raw(font, x, y, text, color, scale),
            TextStyle::Bold => {
                self.text_ttf_raw(font, x, y, text, color, scale);
                self.text_ttf_raw(font, x + (scale / 3).max(1), y, text, color, scale);
            }
            TextStyle::Outline => {
                let offset = (scale / 2).max(1);
                for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1)] {
                    self.text_ttf_raw(font, x + dx * offset, y + dy * offset, text, outline, scale);
                }
                self.text_ttf_raw(font, x, y, text, color, scale);
            }
        }
        true
    }

    fn text_ttf_raw(
        &mut self,
        font: &FontArc,
        x: i32,
        y: i32,
        text: &str,
        color: Rgba,
        scale: i32,
    ) {
        let scale = text_px_scale(scale);
        let scaled = font.as_scaled(scale);
        let mut cursor_x = x as f32;
        let baseline = y as f32 + scaled.ascent();
        let mut previous = None;
        for ch in text.chars() {
            if ch == '\n' {
                cursor_x = x as f32;
                previous = None;
                continue;
            }
            let glyph_id = font.glyph_id(ch);
            if let Some(prev) = previous {
                cursor_x += scaled.kern(prev, glyph_id);
            }
            let glyph = glyph_id.with_scale_and_position(scale, point(cursor_x, baseline));
            if let Some(outlined) = font.outline_glyph(glyph) {
                let bounds = outlined.px_bounds();
                outlined.draw(|gx, gy, coverage| {
                    let px = bounds.min.x.round() as i32 + gx as i32;
                    let py = bounds.min.y.round() as i32 + gy as i32;
                    let alpha = ((color.a as f32 * coverage).round()).clamp(0.0, 255.0) as u8;
                    self.set_pixel(px, py, Rgba::rgba(color.r, color.g, color.b, alpha));
                });
            }
            cursor_x += scaled.h_advance(glyph_id);
            previous = Some(glyph_id);
        }
    }

    fn text_raw(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        let mut cursor_x = x;
        for ch in text.chars() {
            if ch == '\n' {
                cursor_x = x;
                continue;
            }
            if let Some(rows) = glyph_rows(ch) {
                for (row_index, row) in rows.iter().enumerate() {
                    for col in 0..5 {
                        if (row >> (4 - col)) & 1 == 1 {
                            self.fill_rect(
                                cursor_x + col * scale,
                                y + row_index as i32 * scale,
                                cursor_x + (col + 1) * scale,
                                y + (row_index as i32 + 1) * scale,
                                color,
                            );
                        }
                    }
                }
            }
            cursor_x += 6 * scale;
        }
    }

    #[allow(dead_code)]
    pub fn save_png(&self, path: &Path) -> std::io::Result<()> {
        self.save_png_with_dpi(path, None)
    }

    pub fn save_png_with_dpi(&self, path: &Path, dpi: Option<u32>) -> std::io::Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        self.write_png_with_dpi(&mut writer, dpi)
    }

    pub fn encode_png_with_dpi(&self, dpi: Option<u32>) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.write_png_with_dpi(&mut bytes, dpi)?;
        Ok(bytes)
    }

    fn write_png_with_dpi<W: Write>(
        &self,
        writer: &mut W,
        dpi: Option<u32>,
    ) -> std::io::Result<()> {
        writer.write_all(b"\x89PNG\r\n\x1A\n")?;

        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&(self.width as u32).to_be_bytes());
        ihdr.extend_from_slice(&(self.height as u32).to_be_bytes());
        ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
        write_chunk(writer, b"IHDR", &ihdr)?;

        if let Some(dpi) = dpi.filter(|dpi| *dpi > 0) {
            let pixels_per_meter = ((dpi as f64) / 0.0254).round() as u32;
            let mut phys = Vec::with_capacity(9);
            phys.extend_from_slice(&pixels_per_meter.to_be_bytes());
            phys.extend_from_slice(&pixels_per_meter.to_be_bytes());
            phys.push(1);
            write_chunk(writer, b"pHYs", &phys)?;
        }

        let mut raw = Vec::with_capacity((self.width * 4 + 1) * self.height);
        for row in self.pixels.chunks_exact(self.width * 4) {
            raw.push(0);
            raw.extend_from_slice(row);
        }
        let compressed = zlib_store(&raw);
        write_chunk(writer, b"IDAT", &compressed)?;
        write_chunk(writer, b"IEND", &[])?;
        writer.flush()
    }

    fn brush(&mut self, x: i32, y: i32, color: Rgba, width: i32) {
        let radius = (width.max(1) - 1) / 2;
        for yy in (y - radius)..=(y + radius) {
            for xx in (x - radius)..=(x + radius) {
                self.set_pixel(xx, yy, color);
            }
        }
    }

    fn set_pixel(&mut self, x: i32, y: i32, color: Rgba) {
        if x < 0 || y < 0 || x >= self.width as i32 || y >= self.height as i32 {
            return;
        }
        let index = (y as usize * self.width + x as usize) * 4;
        if color.a == 255 {
            self.pixels[index..index + 4].copy_from_slice(&[color.r, color.g, color.b, 255]);
            return;
        }
        let alpha = color.a as u16;
        let inv = 255_u16.saturating_sub(alpha);
        self.pixels[index] =
            ((color.r as u16 * alpha + self.pixels[index] as u16 * inv) / 255) as u8;
        self.pixels[index + 1] =
            ((color.g as u16 * alpha + self.pixels[index + 1] as u16 * inv) / 255) as u8;
        self.pixels[index + 2] =
            ((color.b as u16 * alpha + self.pixels[index + 2] as u16 * inv) / 255) as u8;
        self.pixels[index + 3] = 255;
    }
}

impl WaveformCanvas for Canvas {
    fn fill_rect(&mut self, left: i32, top: i32, right: i32, bottom: i32, color: Rgba) {
        Canvas::fill_rect(self, left, top, right, bottom, color);
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
        Canvas::stroke_rect(self, left, top, right, bottom, color, width);
    }

    fn line(&mut self, x0: i32, y0: i32, x1: i32, y1: i32, color: Rgba, width: i32) {
        Canvas::line(self, x0, y0, x1, y1, color, width);
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
        Canvas::line_clipped(self, x0, y0, x1, y1, color, width, clip);
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
        Canvas::arrow(self, x0, y0, x1, y1, color, head_size, width, style);
    }

    fn text(&mut self, x: i32, y: i32, text: &str, color: Rgba, scale: i32) {
        Canvas::text(self, x, y, text, color, scale);
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
        Canvas::text_styled(self, x, y, text, color, scale, style);
    }
}

fn system_font() -> Option<&'static FontArc> {
    static FONT: OnceLock<Option<FontArc>> = OnceLock::new();
    FONT.get_or_init(load_system_font).as_ref()
}

fn load_system_font() -> Option<FontArc> {
    const FONT_PATHS: [&str; 6] = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\simhei.ttf",
        r"C:\Windows\Fonts\segoeui.ttf",
        r"C:\Windows\Fonts\arial.ttf",
        r"C:\Windows\Fonts\consola.ttf",
        r"C:\Windows\Fonts\simsun.ttc",
    ];
    FONT_PATHS.iter().find_map(|path| {
        fs::read(path)
            .ok()
            .and_then(|bytes| FontArc::try_from_vec(bytes).ok())
    })
}

fn text_px_scale(scale: i32) -> PxScale {
    PxScale::from((8.8 * scale.max(1) as f32).max(8.0))
}

fn ttf_text_width(text: &str, scale: i32) -> Option<i32> {
    let font = system_font()?;
    let scale = text_px_scale(scale);
    let scaled = font.as_scaled(scale);
    let mut width = 0.0_f32;
    let mut previous = None;
    for ch in text.chars() {
        if ch == '\n' {
            break;
        }
        let glyph_id = font.glyph_id(ch);
        if let Some(prev) = previous {
            width += scaled.kern(prev, glyph_id);
        }
        width += scaled.h_advance(glyph_id);
        previous = Some(glyph_id);
    }
    Some(width.ceil() as i32)
}

fn write_chunk(writer: &mut impl Write, chunk_type: &[u8; 4], data: &[u8]) -> std::io::Result<()> {
    writer.write_all(&(data.len() as u32).to_be_bytes())?;
    writer.write_all(chunk_type)?;
    writer.write_all(data)?;
    let mut crc_input = Vec::with_capacity(chunk_type.len() + data.len());
    crc_input.extend_from_slice(chunk_type);
    crc_input.extend_from_slice(data);
    writer.write_all(&crc32(&crc_input).to_be_bytes())
}

fn zlib_store(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 65_535 * 5 + 6);
    out.extend_from_slice(&[0x78, 0x01]);
    let mut remaining = data;
    while !remaining.is_empty() {
        let block_len = remaining.len().min(65_535);
        let final_block = block_len == remaining.len();
        out.push(if final_block { 1 } else { 0 });
        out.extend_from_slice(&(block_len as u16).to_le_bytes());
        out.extend_from_slice(&(!(block_len as u16)).to_le_bytes());
        out.extend_from_slice(&remaining[..block_len]);
        remaining = &remaining[block_len..];
    }
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 255, 255]);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_canvas_can_encode_to_memory_with_dpi() {
        let mut canvas = Canvas::new(16, 12, Rgba::rgb(255, 255, 255));
        canvas.line(0, 0, 15, 11, Rgba::rgb(0, 0, 0), 1);

        let bytes = canvas.encode_png_with_dpi(Some(300)).unwrap();

        assert!(bytes.starts_with(b"\x89PNG\r\n\x1A\n"));
        assert!(bytes.windows(4).any(|chunk| chunk == b"pHYs"));
        assert!(bytes.windows(4).any(|chunk| chunk == b"IEND"));
    }
}

fn adler32(data: &[u8]) -> u32 {
    const MOD: u32 = 65_521;
    let mut a = 1_u32;
    let mut b = 0_u32;
    for byte in data {
        a = (a + *byte as u32) % MOD;
        b = (b + a) % MOD;
    }
    (b << 16) | a
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFF_u32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = 0_u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
    }
    !crc
}

fn clip_code(x: i32, y: i32, clip: ClipRect) -> u8 {
    let mut code = 0_u8;
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

fn clip_line(x0: i32, y0: i32, x1: i32, y1: i32, clip: ClipRect) -> Option<(i32, i32, i32, i32)> {
    let mut x0 = x0 as f64;
    let mut y0 = y0 as f64;
    let mut x1 = x1 as f64;
    let mut y1 = y1 as f64;
    loop {
        let code0 = clip_code(x0.round() as i32, y0.round() as i32, clip);
        let code1 = clip_code(x1.round() as i32, y1.round() as i32, clip);
        if code0 | code1 == 0 {
            return Some((
                x0.round() as i32,
                y0.round() as i32,
                x1.round() as i32,
                y1.round() as i32,
            ));
        }
        if code0 & code1 != 0 {
            return None;
        }
        let code = if code0 != 0 { code0 } else { code1 };
        let (x, y) = if code & 8 != 0 {
            (
                x0 + (x1 - x0) * (clip.bottom as f64 - y0) / (y1 - y0),
                clip.bottom as f64,
            )
        } else if code & 4 != 0 {
            (
                x0 + (x1 - x0) * (clip.top as f64 - y0) / (y1 - y0),
                clip.top as f64,
            )
        } else if code & 2 != 0 {
            (
                clip.right as f64,
                y0 + (y1 - y0) * (clip.right as f64 - x0) / (x1 - x0),
            )
        } else {
            (
                clip.left as f64,
                y0 + (y1 - y0) * (clip.left as f64 - x0) / (x1 - x0),
            )
        };
        if code == code0 {
            x0 = x;
            y0 = y;
        } else {
            x1 = x;
            y1 = y;
        }
    }
}

fn glyph_rows(ch: char) -> Option<[i32; 7]> {
    let c = ch.to_ascii_uppercase();
    Some(match c {
        ' ' => [0, 0, 0, 0, 0, 0, 0],
        '0' => [14, 17, 19, 21, 25, 17, 14],
        '1' => [4, 12, 4, 4, 4, 4, 14],
        '2' => [14, 17, 1, 2, 4, 8, 31],
        '3' => [30, 1, 1, 14, 1, 1, 30],
        '4' => [2, 6, 10, 18, 31, 2, 2],
        '5' => [31, 16, 30, 1, 1, 17, 14],
        '6' => [6, 8, 16, 30, 17, 17, 14],
        '7' => [31, 1, 2, 4, 8, 8, 8],
        '8' => [14, 17, 17, 14, 17, 17, 14],
        '9' => [14, 17, 17, 15, 1, 2, 12],
        'A' => [14, 17, 17, 31, 17, 17, 17],
        'B' => [30, 17, 17, 30, 17, 17, 30],
        'C' => [14, 17, 16, 16, 16, 17, 14],
        'D' => [30, 17, 17, 17, 17, 17, 30],
        'E' => [31, 16, 16, 30, 16, 16, 31],
        'F' => [31, 16, 16, 30, 16, 16, 16],
        'G' => [14, 17, 16, 23, 17, 17, 15],
        'H' => [17, 17, 17, 31, 17, 17, 17],
        'I' => [14, 4, 4, 4, 4, 4, 14],
        'J' => [7, 2, 2, 2, 18, 18, 12],
        'K' => [17, 18, 20, 24, 20, 18, 17],
        'L' => [16, 16, 16, 16, 16, 16, 31],
        'M' => [17, 27, 21, 21, 17, 17, 17],
        'N' => [17, 25, 21, 19, 17, 17, 17],
        'O' => [14, 17, 17, 17, 17, 17, 14],
        'P' => [30, 17, 17, 30, 16, 16, 16],
        'Q' => [14, 17, 17, 17, 21, 18, 13],
        'R' => [30, 17, 17, 30, 20, 18, 17],
        'S' => [15, 16, 16, 14, 1, 1, 30],
        'T' => [31, 4, 4, 4, 4, 4, 4],
        'U' => [17, 17, 17, 17, 17, 17, 14],
        'V' => [17, 17, 17, 17, 17, 10, 4],
        'W' => [17, 17, 17, 21, 21, 21, 10],
        'X' => [17, 17, 10, 4, 10, 17, 17],
        'Y' => [17, 17, 10, 4, 4, 4, 4],
        'Z' => [31, 1, 2, 4, 8, 16, 31],
        '-' => [0, 0, 0, 31, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 31],
        '.' => [0, 0, 0, 0, 0, 12, 12],
        ',' => [0, 0, 0, 0, 0, 12, 8],
        ':' => [0, 12, 12, 0, 12, 12, 0],
        '/' => [1, 1, 2, 4, 8, 16, 16],
        '\\' => [16, 16, 8, 4, 2, 1, 1],
        '(' => [2, 4, 8, 8, 8, 4, 2],
        ')' => [8, 4, 2, 2, 2, 4, 8],
        '[' => [14, 8, 8, 8, 8, 8, 14],
        ']' => [14, 2, 2, 2, 2, 2, 14],
        '+' => [0, 4, 4, 31, 4, 4, 0],
        '=' => [0, 0, 31, 0, 31, 0, 0],
        '%' => [24, 25, 2, 4, 8, 19, 3],
        '#' => [10, 10, 31, 10, 31, 10, 10],
        '?' => [14, 17, 1, 2, 4, 0, 4],
        _ => [14, 17, 1, 6, 4, 0, 4],
    })
}
