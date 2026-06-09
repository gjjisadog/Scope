# Export Annotation Word Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build a screenshot-tool-style waveform export annotation workspace and a Word report export path that batch inserts annotated waveform images into one default `.docx` report.

**Architecture:** Keep the existing waveform renderer, then extract annotation state and Word generation into focused modules. Add manual arrow annotations and Word report export while preserving the current PNG/SVG preview-render-save path.

**Tech Stack:** Rust 2021, eframe/egui 0.27, custom PNG/SVG canvas, new OpenXML `.docx` writer using a small std-only ZIP writer, existing Rust unit tests and manual UI verification.

---

## File Map

- Modify `Cargo.toml`: bump package version.
- Modify `Cargo.lock`: keep the root package version in sync.
- Modify `src/main.rs`: register new modules.
- Create `src/export_annotation.rs`: shared export annotation state, geometry helpers, undo state shape, and tests.
- Modify `src/png_export.rs`: add in-memory PNG encoding for Word embedding.
- Create `src/word_export.rs`: minimal `.docx` OpenXML writer and tests.
- Modify `src/app.rs`: wire annotation model, manual arrow tool, Word export actions, batch Word export, and export workspace layout.
- Modify `README.md`: document the new export workspace and Word report export.
- Modify `scripts/package-windows.ps1`: bump package version.
- Modify `scripts/ScopeAnalyzer.wxs`: bump installer version.

## Task 1: Versioning

**Files:**
- Modify: `Cargo.toml`
- Modify: `scripts/package-windows.ps1`
- Modify: `scripts/ScopeAnalyzer.wxs`
- Generated: `Cargo.lock`

- [ ] **Step 1: Update Cargo metadata**

Change `Cargo.toml`:

```toml
[package]
name = "scope_analyzer"
version = "0.6.0"
edition = "2021"
description = "Windows offline oscilloscope-style waveform analyzer"
license = "Proprietary"

[dependencies]
ab_glyph = "0.2"
csv = "1.4"
eframe = { version = "0.27", default-features = false, features = ["default_fonts", "glow", "wgpu"] }
egui_plot = "0.27"
encoding_rs = "0.8"
rfd = "0.14"
rustfft = "6.2"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
thiserror = "1.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["fmt", "env-filter"] }
```

- [ ] **Step 2: Bump package script version**

Change `scripts/package-windows.ps1`:

```powershell
$version = "0.6.0"
```

- [ ] **Step 3: Bump MSI version**

Change `scripts/ScopeAnalyzer.wxs`:

```xml
Version="0.6.0"
```

- [ ] **Step 4: Sync root package version in lockfile**

Change the root package entry in `Cargo.lock`:

```toml
[[package]]
name = "scope_analyzer"
version = "0.6.0"
```

Expected: `Cargo.toml`, `Cargo.lock`, `scripts/package-windows.ps1`, and `scripts/ScopeAnalyzer.wxs` all show `0.6.0`.

## Task 2: Add Export Annotation Model

**Files:**
- Create: `src/export_annotation.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add core data types**

Create `src/export_annotation.rs`:

```rust
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

#[derive(Clone, Debug, PartialEq)]
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
pub struct InkStroke {
    pub points: Vec<CanvasPoint>,
    pub color: AnnotationColor,
    pub width: i32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AnnotationDocument {
    pub variable_labels: Vec<VariableLabelAnnotation>,
    pub text_annotations: Vec<TextAnnotation>,
    pub arrow_annotations: Vec<ArrowAnnotation>,
    pub ink_strokes: Vec<InkStroke>,
}
```

- [ ] **Step 2: Register the export annotation module**

Add to `src/main.rs` near the other module declarations:

```rust
mod export_annotation;
```

- [ ] **Step 3: Add geometry helpers**

Append:

```rust
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
    CanvasPoint::new(position.x.clamp(min_x, max_x), position.y.clamp(min_y, max_y))
}

pub fn arrow_start_for_label(label: CanvasRect, target: CanvasPoint) -> CanvasPoint {
    let center_y = label.top + label.height() / 2;
    if target.x < label.left {
        CanvasPoint::new(label.left - 5, center_y)
    } else {
        CanvasPoint::new(label.right + 5, center_y)
    }
}

pub fn point_segment_distance_sq(
    point: CanvasPoint,
    start: CanvasPoint,
    end: CanvasPoint,
) -> f64 {
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
```

- [ ] **Step 4: Add tests**

Append:

```rust
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
}
```

- [ ] **Step 5: Run focused tests**

Run:

```bash
cargo test export_annotation
```

Expected: three new annotation geometry tests pass.

## Task 3: Add In-Memory PNG Encoding

**Files:**
- Modify: `src/png_export.rs`

- [ ] **Step 1: Factor PNG writing into a reusable writer**

Change `save_png_with_dpi` and add `encode_png_with_dpi`:

```rust
    pub fn encode_png_with_dpi(&self, dpi: Option<u32>) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        self.write_png_with_dpi(&mut bytes, dpi)?;
        Ok(bytes)
    }

    pub fn save_png_with_dpi(&self, path: &Path, dpi: Option<u32>) -> std::io::Result<()> {
        let file = File::create(path)?;
        let mut writer = BufWriter::new(file);
        self.write_png_with_dpi(&mut writer, dpi)
    }

    fn write_png_with_dpi<W: Write>(&self, writer: &mut W, dpi: Option<u32>) -> std::io::Result<()> {
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
```

- [ ] **Step 2: Add a PNG byte smoke test**

Add a test near existing PNG/perf smoke coverage:

```rust
#[test]
fn png_canvas_can_encode_to_memory_with_dpi() {
    let mut canvas = Canvas::new(16, 12, Rgba::rgb(255, 255, 255));
    canvas.line(0, 0, 15, 11, Rgba::rgb(0, 0, 0), 1);
    let bytes = canvas.encode_png_with_dpi(Some(300)).unwrap();
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1A\n"));
    assert!(bytes.windows(4).any(|chunk| chunk == b"pHYs"));
    assert!(bytes.windows(4).any(|chunk| chunk == b"IEND"));
}
```

- [ ] **Step 3: Run focused tests**

Run:

```bash
cargo test png_canvas_can_encode_to_memory_with_dpi
```

Expected: memory PNG encoding test passes.

## Task 4: Add Word Report Writer

**Files:**
- Create: `src/word_export.rs`
- Modify: `src/main.rs`

- [ ] **Step 1: Add report data model**

Create `src/word_export.rs`:

```rust
use std::{
    fs::File,
    io::{Cursor, Seek, Write},
    path::Path,
};

use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorTable {
    pub headers: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordReportFigure {
    pub caption: String,
    pub png: Vec<u8>,
    pub width_px: usize,
    pub height_px: usize,
    pub cursor_table: Option<CursorTable>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WordReport {
    pub title: String,
    pub experiment_name: String,
    pub source_name: String,
    pub exported_at: String,
    pub sample_rate: Option<String>,
    pub time_range_summary: String,
    pub include_cursor_tables: bool,
    pub figures: Vec<WordReportFigure>,
}

#[derive(Debug, Error)]
pub enum WordExportError {
    #[error("report has no figures")]
    EmptyReport,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
```

- [ ] **Step 2: Register the Word export module**

Add to `src/main.rs` near the other module declarations:

```rust
mod word_export;
```

- [ ] **Step 3: Add public writer functions**

Append:

```rust
pub fn write_word_report(path: &Path, report: &WordReport) -> Result<(), WordExportError> {
    let file = File::create(path)?;
    write_word_report_to_writer(file, report)
}

pub fn write_word_report_to_vec(report: &WordReport) -> Result<Vec<u8>, WordExportError> {
    let mut cursor = Cursor::new(Vec::new());
    write_word_report_to_writer(&mut cursor, report)?;
    Ok(cursor.into_inner())
}

fn write_word_report_to_writer<W: Write + Seek>(
    writer: W,
    report: &WordReport,
) -> Result<(), WordExportError> {
    if report.figures.is_empty() {
        return Err(WordExportError::EmptyReport);
    }

    let mut zip = SimpleZipWriter::new(writer);
    zip.add_file("[Content_Types].xml", content_types(report).as_bytes())?;
    zip.add_file("_rels/.rels", root_rels().as_bytes())?;
    zip.add_file("word/_rels/document.xml.rels", document_rels(report).as_bytes())?;
    zip.add_file("word/document.xml", document_xml(report).as_bytes())?;

    for (index, figure) in report.figures.iter().enumerate() {
        zip.add_file(&format!("word/media/image{}.png", index + 1), &figure.png)?;
    }

    zip.finish()?;
    Ok(())
}
```

- [ ] **Step 4: Add std-only ZIP helpers**

Append a minimal stored-entry ZIP writer:

```rust
struct ZipEntry {
    name: String,
    crc32: u32,
    size: u32,
    local_header_offset: u32,
}

struct SimpleZipWriter<W: Write + Seek> {
    writer: W,
    entries: Vec<ZipEntry>,
}

impl<W: Write + Seek> SimpleZipWriter<W> {
    fn new(writer: W) -> Self {
        Self {
            writer,
            entries: Vec::new(),
        }
    }

    fn add_file(&mut self, name: &str, data: &[u8]) -> std::io::Result<()> {
        let offset = self.writer.stream_position()? as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        write_u32(&mut self.writer, 0x0403_4b50)?;
        write_u16(&mut self.writer, 20)?;
        write_u16(&mut self.writer, 0)?;
        write_u16(&mut self.writer, 0)?;
        write_u16(&mut self.writer, 0)?;
        write_u16(&mut self.writer, 0)?;
        write_u32(&mut self.writer, crc)?;
        write_u32(&mut self.writer, size)?;
        write_u32(&mut self.writer, size)?;
        write_u16(&mut self.writer, name.len() as u16)?;
        write_u16(&mut self.writer, 0)?;
        self.writer.write_all(name.as_bytes())?;
        self.writer.write_all(data)?;
        self.entries.push(ZipEntry {
            name: name.to_owned(),
            crc32: crc,
            size,
            local_header_offset: offset,
        });
        Ok(())
    }

    fn finish(mut self) -> std::io::Result<()> {
        let central_start = self.writer.stream_position()? as u32;
        for entry in &self.entries {
            write_u32(&mut self.writer, 0x0201_4b50)?;
            write_u16(&mut self.writer, 20)?;
            write_u16(&mut self.writer, 20)?;
            write_u16(&mut self.writer, 0)?;
            write_u16(&mut self.writer, 0)?;
            write_u16(&mut self.writer, 0)?;
            write_u16(&mut self.writer, 0)?;
            write_u32(&mut self.writer, entry.crc32)?;
            write_u32(&mut self.writer, entry.size)?;
            write_u32(&mut self.writer, entry.size)?;
            write_u16(&mut self.writer, entry.name.len() as u16)?;
            write_u16(&mut self.writer, 0)?;
            write_u16(&mut self.writer, 0)?;
            write_u16(&mut self.writer, 0)?;
            write_u16(&mut self.writer, 0)?;
            write_u32(&mut self.writer, 0)?;
            write_u32(&mut self.writer, entry.local_header_offset)?;
            self.writer.write_all(entry.name.as_bytes())?;
        }
        let central_end = self.writer.stream_position()? as u32;
        let central_size = central_end - central_start;
        write_u32(&mut self.writer, 0x0605_4b50)?;
        write_u16(&mut self.writer, 0)?;
        write_u16(&mut self.writer, 0)?;
        write_u16(&mut self.writer, self.entries.len() as u16)?;
        write_u16(&mut self.writer, self.entries.len() as u16)?;
        write_u32(&mut self.writer, central_size)?;
        write_u32(&mut self.writer, central_start)?;
        write_u16(&mut self.writer, 0)?;
        self.writer.flush()
    }
}

fn write_u16<W: Write>(writer: &mut W, value: u16) -> std::io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn write_u32<W: Write>(writer: &mut W, value: u32) -> std::io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff_u32;
    for byte in data {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = if crc & 1 == 1 { 0xedb8_8320 } else { 0 };
            crc = (crc >> 1) ^ mask;
        }
    }
    !crc
}
```

- [ ] **Step 5: Add OpenXML helpers**

Append helpers that generate valid minimal WordprocessingML:

```rust
const EMU_PER_INCH: i64 = 914_400;

fn image_size_emu(width_px: usize, height_px: usize) -> (i64, i64) {
    let width_in = 6.5_f64;
    let height_in = width_in * height_px as f64 / width_px.max(1) as f64;
    (
        (width_in * EMU_PER_INCH as f64).round() as i64,
        (height_in * EMU_PER_INCH as f64).round() as i64,
    )
}

fn content_types(report: &WordReport) -> String {
    let mut overrides = String::new();
    for index in 0..report.figures.len() {
        overrides.push_str(&format!(
            r#"<Override PartName="/word/media/image{}.png" ContentType="image/png"/>"#,
            index + 1
        ));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="png" ContentType="image/png"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
{overrides}
</Types>"#
    )
}

fn root_rels() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"#
}

fn document_rels(report: &WordReport) -> String {
    let mut rels = String::from(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
    );
    for index in 0..report.figures.len() {
        rels.push_str(&format!(
            r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="media/image{}.png"/>"#,
            index + 1,
            index + 1
        ));
    }
    rels.push_str("</Relationships>");
    rels
}

fn document_xml(report: &WordReport) -> String {
    let mut body = String::new();
    body.push_str(&paragraph(&report.title, true));
    body.push_str(&paragraph(&format!("实验名称：{}", report.experiment_name), false));
    body.push_str(&paragraph(&format!("数据文件：{}", report.source_name), false));
    body.push_str(&paragraph(&format!("导出时间：{}", report.exported_at), false));
    if let Some(sample_rate) = &report.sample_rate {
        body.push_str(&paragraph(&format!("采样率：{sample_rate}"), false));
    }
    body.push_str(&paragraph(&format!("时间范围：{}", report.time_range_summary), false));

    for (index, figure) in report.figures.iter().enumerate() {
        body.push_str(&image_paragraph(index + 1, figure));
        body.push_str(&paragraph(&figure.caption, false));
        if report.include_cursor_tables {
            if let Some(table) = &figure.cursor_table {
                body.push_str(&table_xml(table));
            }
        }
    }

    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"
 xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"
 xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing"
 xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main"
 xmlns:pic="http://schemas.openxmlformats.org/drawingml/2006/picture">
<w:body>{body}<w:sectPr><w:pgSz w:w="12240" w:h="15840"/><w:pgMar w:top="720" w:right="720" w:bottom="720" w:left="720" w:header="360" w:footer="360" w:gutter="0"/></w:sectPr></w:body>
</w:document>"#
    )
}
```

- [ ] **Step 6: Add XML element helpers**

Append:

```rust
fn paragraph(text: &str, bold: bool) -> String {
    let bold_xml = if bold { "<w:b/>" } else { "" };
    format!(
        r#"<w:p><w:r><w:rPr>{bold_xml}</w:rPr><w:t>{}</w:t></w:r></w:p>"#,
        escape_xml(text)
    )
}

fn image_paragraph(index: usize, figure: &WordReportFigure) -> String {
    let (cx, cy) = image_size_emu(figure.width_px, figure.height_px);
    format!(
        r#"<w:p><w:r><w:drawing><wp:inline><wp:extent cx="{cx}" cy="{cy}"/><wp:docPr id="{index}" name="Waveform {index}"/><a:graphic><a:graphicData uri="http://schemas.openxmlformats.org/drawingml/2006/picture"><pic:pic><pic:nvPicPr><pic:cNvPr id="{index}" name="waveform{index}.png"/><pic:cNvPicPr/></pic:nvPicPr><pic:blipFill><a:blip r:embed="rId{index}"/><a:stretch><a:fillRect/></a:stretch></pic:blipFill><pic:spPr><a:xfrm><a:off x="0" y="0"/><a:ext cx="{cx}" cy="{cy}"/></a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></pic:spPr></pic:pic></a:graphicData></a:graphic></wp:inline></w:drawing></w:r></w:p>"#
    )
}

fn table_xml(table: &CursorTable) -> String {
    let mut xml = String::from("<w:tbl>");
    if !table.headers.is_empty() {
        xml.push_str("<w:tr>");
        for header in &table.headers {
            xml.push_str(&table_cell(header));
        }
        xml.push_str("</w:tr>");
    }
    for row in &table.rows {
        xml.push_str("<w:tr>");
        for cell in row {
            xml.push_str(&table_cell(cell));
        }
        xml.push_str("</w:tr>");
    }
    xml.push_str("</w:tbl>");
    xml
}

fn table_cell(text: &str) -> String {
    format!(r#"<w:tc><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:tc>"#, escape_xml(text))
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
```

- [ ] **Step 7: Add Word writer tests**

Append:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sample_png() -> Vec<u8> {
        b"\x89PNG\r\n\x1A\nsample".to_vec()
    }

    fn zip_contains_file(bytes: &[u8], name: &str) -> bool {
        bytes.windows(name.len()).any(|window| window == name.as_bytes())
    }

    fn sample_report(include_cursor_tables: bool) -> WordReport {
        WordReport {
            title: "实验波形报告".to_owned(),
            experiment_name: "sample".to_owned(),
            source_name: "sample.csv".to_owned(),
            exported_at: "2026-06-09 12:00:00".to_owned(),
            sample_rate: Some("1000 Hz".to_owned()),
            time_range_summary: "0.000000s - 1.000000s".to_owned(),
            include_cursor_tables,
            figures: vec![WordReportFigure {
                caption: "图 1：窗口 #1 0.000000s - 1.000000s".to_owned(),
                png: sample_png(),
                width_px: 1600,
                height_px: 900,
                cursor_table: Some(CursorTable {
                    headers: vec!["变量".to_owned(), "Y@X1".to_owned()],
                    rows: vec![vec!["Ia".to_owned(), "1.234".to_owned()]],
                }),
            }],
        }
    }

    #[test]
    fn word_report_contains_required_parts_and_image_relationship() {
        let bytes = write_word_report_to_vec(&sample_report(true)).unwrap();
        assert!(bytes.starts_with(b"PK"));
        assert!(zip_contains_file(&bytes, "[Content_Types].xml"));
        assert!(zip_contains_file(&bytes, "word/document.xml"));
        assert!(zip_contains_file(&bytes, "word/_rels/document.xml.rels"));
        assert!(zip_contains_file(&bytes, "word/media/image1.png"));
    }

    #[test]
    fn word_report_cursor_table_respects_toggle() {
        let with_table = document_xml(&sample_report(true));
        assert!(with_table.contains("Y@X1"));
        let without_table = document_xml(&sample_report(false));
        assert!(!without_table.contains("Y@X1"));
    }

    #[test]
    fn word_report_rejects_empty_reports() {
        let mut report = sample_report(false);
        report.figures.clear();
        assert!(matches!(
            write_word_report_to_vec(&report),
            Err(WordExportError::EmptyReport)
        ));
    }
}
```

- [ ] **Step 8: Run focused tests**

Run:

```bash
cargo test word_export
```

Expected: Word writer tests pass and generated archive contains required `.docx` parts.

## Task 5: Wire Manual Arrow Annotations Into App State And Rendering

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Add app-level arrow structs and drag state**

Near existing export preview structs, add:

```rust
#[derive(Clone, Debug, PartialEq)]
struct ExportArrowAnnotation {
    start: [i32; 2],
    end: [i32; 2],
    color: Color32,
    width: i32,
    head_size: f32,
    line_style: ExportArrowLineStyle,
}

#[derive(Clone, Debug)]
struct ExportArrowDrag {
    arrow_index: Option<usize>,
    before_state: ExportPreviewEditState,
    undo_recorded: bool,
}
```

- [ ] **Step 2: Extend tool enum**

Change:

```rust
enum ExportPreviewTool {
    Select,
    Text,
    Arrow,
    Brush,
    Eraser,
}
```

- [ ] **Step 3: Add state fields to `ScopeApp`**

Add fields next to `export_text_annotations` and `export_ink_strokes`:

```rust
    export_arrow_annotations: Vec<ExportArrowAnnotation>,
    export_arrow_drag: Option<ExportArrowDrag>,
```

Initialize in `Default`:

```rust
            export_arrow_annotations: Vec::new(),
            export_arrow_drag: None,
```

- [ ] **Step 4: Include arrows in undo snapshots**

Add to `ExportPreviewEditState`:

```rust
    arrow_annotations: Vec<ExportArrowAnnotation>,
```

Update `export_preview_state`:

```rust
            arrow_annotations: self.export_arrow_annotations.clone(),
```

Update `restore_export_preview_state`:

```rust
        self.export_arrow_annotations = state.arrow_annotations;
        self.export_arrow_drag = None;
```

- [ ] **Step 5: Draw manual arrows**

Add a helper:

```rust
    fn draw_export_arrow_annotations<C: WaveformCanvas>(&self, canvas: &mut C) {
        for arrow in &self.export_arrow_annotations {
            let color = Self::export_color(arrow.color);
            let width = arrow.width.max(1);
            let style = arrow.line_style.stroke_style();
            canvas.arrow(
                arrow.start[0],
                arrow.start[1],
                arrow.end[0],
                arrow.end[1],
                color,
                arrow.head_size.clamp(MIN_EXPORT_ARROW_SIZE, MAX_EXPORT_ARROW_SIZE),
                width,
                style,
            );
        }
    }
```

Call it in both `render_current_waveform_canvas_with_layout` and `render_current_waveform_svg` after text annotations and before ink strokes:

```rust
        self.draw_export_text_annotations(&mut canvas);
        self.draw_export_arrow_annotations(&mut canvas);
        self.draw_export_ink_strokes(&mut canvas);
```

- [ ] **Step 6: Add arrow creation interaction**

Add:

```rust
    fn export_preview_arrow_interactions(
        &mut self,
        ui: &mut egui::Ui,
        image_rect: egui::Rect,
        scale: f32,
        ctx: &egui::Context,
    ) {
        let id = ui.id().with("export_preview_arrow_tool");
        let response = ui.interact(image_rect, id, egui::Sense::click_and_drag());
        if response.hovered() {
            ui.output_mut(|output| output.cursor_icon = egui::CursorIcon::Crosshair);
        }
        if response.drag_started_by(PointerButton::Primary) {
            let Some(pointer_pos) = response.interact_pointer_pos() else {
                return;
            };
            let canvas_pos = Self::preview_pos_to_canvas(image_rect, scale, pointer_pos);
            let before = self.export_preview_state();
            self.export_arrow_annotations.push(ExportArrowAnnotation {
                start: canvas_pos,
                end: canvas_pos,
                color: self.export_brush_color,
                width: self.export_brush_width.clamp(1, 32),
                head_size: self.export_arrow_size,
                line_style: self.export_arrow_line_style,
            });
            self.export_arrow_drag = Some(ExportArrowDrag {
                arrow_index: self.export_arrow_annotations.len().checked_sub(1),
                before_state: before,
                undo_recorded: false,
            });
        }
        if response.dragged_by(PointerButton::Primary) {
            let Some(pointer_pos) = response.interact_pointer_pos() else {
                return;
            };
            let canvas_pos = Self::preview_pos_to_canvas(image_rect, scale, pointer_pos);
            let before = self.export_arrow_drag.as_mut().and_then(|drag| {
                if !drag.undo_recorded {
                    drag.undo_recorded = true;
                    Some(drag.before_state.clone())
                } else {
                    None
                }
            });
            if let Some(index) = self.export_arrow_drag.as_ref().and_then(|drag| drag.arrow_index) {
                if let Some(arrow) = self.export_arrow_annotations.get_mut(index) {
                    arrow.end = canvas_pos;
                }
            }
            if let Some(before) = before {
                self.push_export_preview_undo(before);
            }
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
        if response.drag_stopped_by(PointerButton::Primary) {
            self.export_arrow_drag = None;
        }
    }
```

In `export_preview_image_interactions`, route the tool before brush:

```rust
        if self.export_preview_tool == ExportPreviewTool::Arrow {
            self.export_preview_arrow_interactions(ui, image_rect, scale, ctx);
            return;
        }
```

- [ ] **Step 7: Run focused tests**

Run:

```bash
cargo test export_text_annotation_uses_selected_canvas_position
```

Expected: existing export annotation tests still pass after state snapshot changes.

## Task 6: Reorganize Export Preview UI Into Annotation Workspace

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Extract export settings controls**

Create helper from the top settings row currently inside `export_preview_window`:

```rust
    fn export_preview_settings_bar(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut changed = false;
        let before_controls = self.export_preview_state();
        let previous_resolution = self.export_resolution;
        let previous_dpi_value = self.export_dpi_value();
        let previous_pane_scope = self.export_pane_scope;
        let previous_time_range_mode = self.export_time_range_mode;
        let previous_manual_range = (self.export_manual_start, self.export_manual_end);
        let previous_cursor_table_enabled = self.export_cursor_table_enabled;

        ui.horizontal_wrapped(|ui| {
            ui.label(self.tr("子窗口", "Pane"));
            egui::ComboBox::from_id_source("export_preview_pane_scope")
                .selected_text(self.export_pane_scope.label(self.language))
                .show_ui(ui, |ui| {
                    for scope in ExportPaneScope::ALL {
                        changed |= ui
                            .selectable_value(&mut self.export_pane_scope, scope, scope.label(self.language))
                            .changed();
                    }
                });
            ui.label(self.tr("时间范围", "Time range"));
            egui::ComboBox::from_id_source("export_preview_time_range")
                .selected_text(self.export_time_range_mode.label(self.language))
                .show_ui(ui, |ui| {
                    for mode in ExportTimeRangeMode::ALL {
                        changed |= ui
                            .selectable_value(&mut self.export_time_range_mode, mode, mode.label(self.language))
                            .changed();
                    }
                });
            if self.export_time_range_mode == ExportTimeRangeMode::Manual {
                changed |= ui.add(egui::DragValue::new(&mut self.export_manual_start).speed(0.0001).prefix("X0 ")).changed();
                changed |= ui.add(egui::DragValue::new(&mut self.export_manual_end).speed(0.0001).prefix("X1 ")).changed();
            }
            ui.label(self.tr("分辨率", "Resolution"));
            egui::ComboBox::from_id_source("export_preview_resolution")
                .selected_text(self.export_resolution.label(self.language))
                .show_ui(ui, |ui| {
                    for resolution in ExportResolution::ALL {
                        changed |= ui
                            .selectable_value(&mut self.export_resolution, resolution, resolution.label(self.language))
                            .changed();
                    }
                });
            ui.label("DPI");
            changed |= self.export_dpi_controls(ui, "export_preview_dpi");
            let cursor_table_label = self.tr("光标数据表", "Cursor Table");
            changed |= ui.checkbox(&mut self.export_cursor_table_enabled, cursor_table_label).changed();
        });

        if changed {
            if self.export_resolution != previous_resolution
                || self.export_dpi_value() != previous_dpi_value
                || self.export_pane_scope != previous_pane_scope
                || self.export_time_range_mode != previous_time_range_mode
                || (self.export_manual_start, self.export_manual_end) != previous_manual_range
                || self.export_cursor_table_enabled != previous_cursor_table_enabled
            {
                self.export_label_positions.fill(None);
                self.export_label_anchor_x.fill(None);
            }
            if self.export_pane_scope != previous_pane_scope {
                self.export_label_overrides.clear();
                self.export_label_positions.clear();
                self.export_label_anchor_x.clear();
            }
            self.push_export_preview_undo(before_controls);
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
    }
```

- [ ] **Step 2: Add left tool strip helper**

```rust
    fn export_preview_tool_strip(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.vertical_centered(|ui| {
            ui.selectable_value(&mut self.export_preview_tool, ExportPreviewTool::Select, "↖")
                .on_hover_text(self.tr("选择", "Select"));
            if ui
                .selectable_label(self.export_preview_tool == ExportPreviewTool::Text, "T")
                .on_hover_text(self.tr("文字", "Text"))
                .clicked()
            {
                self.export_preview_tool = ExportPreviewTool::Text;
                ctx.request_repaint();
            }
            ui.selectable_value(&mut self.export_preview_tool, ExportPreviewTool::Arrow, "→")
                .on_hover_text(self.tr("箭头", "Arrow"));
            ui.selectable_value(&mut self.export_preview_tool, ExportPreviewTool::Brush, "✎")
                .on_hover_text(self.tr("画笔", "Brush"));
            ui.selectable_value(&mut self.export_preview_tool, ExportPreviewTool::Eraser, "⌫")
                .on_hover_text(self.tr("橡皮", "Eraser"));
            ui.separator();
            if ui.add_enabled(!self.export_preview_undo_stack.is_empty(), egui::Button::new("↶"))
                .on_hover_text(self.tr("撤销", "Undo"))
                .clicked()
            {
                self.undo_export_preview_edit();
                ctx.request_repaint();
            }
            if ui.add_enabled(!self.export_preview_redo_stack.is_empty(), egui::Button::new("↷"))
                .on_hover_text(self.tr("重做", "Redo"))
                .clicked()
            {
                self.redo_export_preview_edit();
                ctx.request_repaint();
            }
        });
    }
```

- [ ] **Step 3: Add right property panel helper**

```rust
    fn export_preview_property_panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading(self.tr("标注属性", "Annotation"));
        let before = self.export_preview_state();
        let mut changed = false;

        ui.label(self.tr("文字/变量字号", "Text size"));
        changed |= ui
            .add(egui::Slider::new(&mut self.export_label_scale, MIN_EXPORT_LABEL_SCALE..=MAX_EXPORT_LABEL_SCALE))
            .changed();
        ui.label(self.tr("箭头大小", "Arrow size"));
        changed |= ui
            .add(egui::Slider::new(&mut self.export_arrow_size, MIN_EXPORT_ARROW_SIZE..=MAX_EXPORT_ARROW_SIZE))
            .changed();
        ui.label(self.tr("笔宽", "Pen width"));
        changed |= ui
            .add(egui::DragValue::new(&mut self.export_brush_width).clamp_range(1..=32).speed(1))
            .changed();
        ui.label(self.tr("颜色", "Color"));
        changed |= egui::color_picker::color_edit_button_srgba(
            ui,
            &mut self.export_brush_color,
            egui::color_picker::Alpha::Opaque,
        )
        .changed();

        ui.separator();
        ui.label(self.tr("变量箭头颜色", "Variable arrow color"));
        egui::ComboBox::from_id_source("export_preview_color")
            .selected_text(self.export_arrow_color_style.label(self.language))
            .show_ui(ui, |ui| {
                for style in ExportArrowColorStyle::ALL {
                    changed |= ui
                        .selectable_value(&mut self.export_arrow_color_style, style, style.label(self.language))
                        .changed();
                }
            });
        changed |= self.export_arrow_style_controls(ui, "export_preview_arrow_line");
        egui::ComboBox::from_id_source("export_preview_font")
            .selected_text(self.export_label_font_style.label(self.language))
            .show_ui(ui, |ui| {
                for style in ExportLabelFontStyle::ALL {
                    changed |= ui
                        .selectable_value(&mut self.export_label_font_style, style, style.label(self.language))
                        .changed();
                }
            });

        if changed {
            self.push_export_preview_undo(before);
            self.mark_export_preview_dirty();
            ctx.request_repaint();
        }
    }
```

- [ ] **Step 4: Replace preview layout body**

Inside `export_preview_window`, after `self.export_preview_settings_bar(ui, ctx);`, lay out the workspace:

```rust
                ui.separator();
                ui.horizontal(|ui| {
                    ui.set_min_height(540.0);
                    ui.vertical(|ui| {
                        self.export_preview_tool_strip(ui, ctx);
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        self.export_preview_canvas(ui, ctx);
                    });
                    ui.separator();
                    ui.vertical(|ui| {
                        ui.set_width(260.0);
                        self.export_preview_property_panel(ui, ctx);
                    });
                });
                ui.separator();
                self.export_preview_actions(ui);
```

Create `export_preview_canvas` by moving the existing texture/error/scroll area block unchanged. Create `export_preview_actions` by moving the existing save PNG/SVG buttons and adding Word in Task 8.

- [ ] **Step 5: Run compile check**

Run:

```bash
cargo check
```

Expected: export preview compiles with the reorganized helpers and arrow tool.

## Task 7: Build Cursor Table Report Data

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Add report metadata helpers**

Add:

```rust
    fn export_report_source_name(&self) -> String {
        self.loaded_path
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .filter(|name| !name.trim().is_empty())
            .or_else(|| self.meta().map(|meta| meta.source_name.as_str()))
            .unwrap_or("waveform")
            .to_owned()
    }

    fn export_report_sample_rate(&self) -> Option<String> {
        self.sample_rate_hz
            .is_finite()
            .then(|| format!("{:.3} Hz", self.sample_rate_hz))
    }
```

- [ ] **Step 2: Add cursor table data helper**

Add:

```rust
    fn export_cursor_table_for_report(
        &self,
        selections: &PlotSelections,
        source_pane_count: usize,
    ) -> Option<crate::word_export::CursorTable> {
        if !self.export_cursor_table_enabled || !(self.show_cursor_a || self.show_cursor_b) {
            return None;
        }
        let labels = self.current_export_curve_labels(selections);
        if labels.is_empty() {
            return None;
        }
        let mut headers = vec![self.tr("变量", "Variable").to_owned()];
        if self.show_cursor_a {
            headers.push("Y@X1".to_owned());
        }
        if self.show_cursor_b {
            headers.push("Y@X2".to_owned());
        }
        if self.show_cursor_a && self.show_cursor_b {
            headers.push("ΔY".to_owned());
        }

        let mut rows = Vec::new();
        for label in labels {
            rows.push(vec![label.name]);
        }
        let _ = source_pane_count;
        Some(crate::word_export::CursorTable { headers, rows })
    }
```

This first implementation records variable names in the Word cursor table and keeps numerical cursor values in the rendered image table. Numerical table extraction is outside this implementation plan because the rendered image already contains the detailed cursor values when the cursor table option is enabled.

- [ ] **Step 3: Use Word-level toggle tests for cursor table behavior**

Do not add a heavy `ScopeApp` constructor test for this helper. The required automated coverage is the `word_report_cursor_table_respects_toggle` test in `src/word_export.rs`, and manual UI verification covers app wiring.

- [ ] **Step 4: Run tests**

Run:

```bash
cargo test word_report_cursor_table_respects_toggle
```

Expected: Word-level cursor table toggle remains covered.

## Task 8: Add Single And Batch Word Export Plumbing

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: Add default Word filename**

Add near image filename helpers:

```rust
    fn default_waveform_docx_name(&self) -> String {
        let stem = self
            .loaded_path
            .as_ref()
            .and_then(|path| path.file_stem())
            .and_then(|stem| stem.to_str())
            .filter(|stem| !stem.trim().is_empty())
            .or_else(|| self.meta().map(|meta| meta.source_name.as_str()))
            .unwrap_or("waveform");
        format!("{stem}_waveform_report.docx")
    }
```

- [ ] **Step 2: Add Word report figure builder**

Add:

```rust
    fn build_word_report_figure(
        &self,
        caption: String,
        selections: &PlotSelections,
    ) -> Result<crate::word_export::WordReportFigure, String> {
        let canvas = self.render_current_waveform_canvas(selections)?;
        let size = canvas.size();
        let png = canvas
            .encode_png_with_dpi(Some(self.export_dpi_value()))
            .map_err(|error| error.to_string())?;
        let pane_count = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS)
            * self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        Ok(crate::word_export::WordReportFigure {
            caption,
            png,
            width_px: size[0],
            height_px: size[1],
            cursor_table: self.export_cursor_table_for_report(selections, pane_count),
        })
    }
```

- [ ] **Step 3: Add report builder**

Add:

```rust
    fn build_word_report(
        &self,
        figures: Vec<crate::word_export::WordReportFigure>,
        range_summary: String,
    ) -> crate::word_export::WordReport {
        let exported_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| format!("Unix {}", duration.as_secs()))
            .unwrap_or_else(|_| "Unknown".to_owned());
        let source_name = self.export_report_source_name();
        crate::word_export::WordReport {
            title: "实验波形报告".to_owned(),
            experiment_name: source_name.clone(),
            source_name,
            exported_at,
            sample_rate: self.export_report_sample_rate(),
            time_range_summary: range_summary,
            include_cursor_tables: self.export_cursor_table_enabled,
            figures,
        }
    }
```

- [ ] **Step 4: Add single-preview Word save**

Add:

```rust
    fn save_export_preview_word(&mut self) {
        let selections = self.current_plot_selections();
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter(self.tr("Word 报告", "Word report"), &["docx"])
            .set_file_name(self.default_waveform_docx_name())
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("docx");
        }
        let range = match self.export_time_range() {
            Ok((start, end)) => format!("{start:.6}s - {end:.6}s"),
            Err(error) => {
                self.export_preview_error = Some(error);
                return;
            }
        };
        let figure = match self.build_word_report_figure(
            format!("图 1：当前波形 {range}"),
            &selections,
        ) {
            Ok(figure) => figure,
            Err(error) => {
                self.export_preview_error = Some(error.clone());
                self.last_error = Some(format!("导出 Word 报告失败: {error}"));
                return;
            }
        };
        let report = self.build_word_report(vec![figure], range);
        if let Err(error) = crate::word_export::write_word_report(&path, &report) {
            let message = error.to_string();
            self.export_preview_error = Some(message.clone());
            self.last_error = Some(format!("导出 Word 报告失败: {message}"));
        }
    }
```

- [ ] **Step 5: Add batch Word export**

Mirror `run_batch_waveform_png_export`, but collect figures and save one file:

```rust
    fn run_batch_waveform_word_export(&mut self) {
        let windows = self.enabled_batch_export_windows();
        if windows.is_empty() {
            self.batch_export_last_summary = Some(self.tr("请至少启用一个时间窗口。", "Enable at least one time window.").to_owned());
            return;
        }
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter(self.tr("Word 报告", "Word report"), &["docx"])
            .set_file_name(self.default_waveform_docx_name())
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension("docx");
        }

        let saved_time_range_mode = self.export_time_range_mode;
        let saved_manual_start = self.export_manual_start;
        let saved_manual_end = self.export_manual_end;
        let saved_pane_scope = self.export_pane_scope;
        let saved_active_pane = self.active_scope_pane;
        let saved_label_overrides = self.export_label_overrides.clone();
        let saved_label_positions = self.export_label_positions.clone();
        let saved_label_anchor_x = self.export_label_anchor_x.clone();

        self.export_time_range_mode = ExportTimeRangeMode::Manual;
        self.export_label_overrides.clear();
        self.export_label_positions.clear();
        self.export_label_anchor_x.clear();

        let source_pane_count = self.scope_layout_rows.clamp(1, MAX_SCOPE_LAYOUT_ROWS)
            * self.scope_layout_cols.clamp(1, MAX_SCOPE_LAYOUT_COLS);
        let pane_jobs = self.batch_export_pane_jobs(source_pane_count);
        let dataset_jobs = self.batch_export_dataset_jobs();
        let mut figures = Vec::new();

        'build: for (window_index, start, end) in windows {
            self.export_manual_start = start;
            self.export_manual_end = end;
            for (dataset_slug, selections) in &dataset_jobs {
                if Self::plot_selection_curve_count(selections) == 0 {
                    continue;
                }
                for (pane_slug, pane_scope, active_pane) in &pane_jobs {
                    self.export_pane_scope = *pane_scope;
                    self.active_scope_pane = (*active_pane).min(source_pane_count.saturating_sub(1));
                    let caption = format!(
                        "图 {}：窗口 #{window_index} {start:.6}s - {end:.6}s {dataset_slug} {pane_slug}",
                        figures.len() + 1
                    );
                    match self.build_word_report_figure(caption, selections) {
                        Ok(figure) => figures.push(figure),
                        Err(error) => {
                            self.batch_export_last_summary = Some(format!("Word 导出失败: {error}"));
                            break 'build;
                        }
                    }
                }
            }
        }

        self.export_time_range_mode = saved_time_range_mode;
        self.export_manual_start = saved_manual_start;
        self.export_manual_end = saved_manual_end;
        self.export_pane_scope = saved_pane_scope;
        self.active_scope_pane = saved_active_pane;
        self.export_label_overrides = saved_label_overrides;
        self.export_label_positions = saved_label_positions;
        self.export_label_anchor_x = saved_label_anchor_x;

        if figures.is_empty() {
            self.batch_export_last_summary = Some(self.tr("没有可导出的波形图。", "No figures to export.").to_owned());
            return;
        }
        let range_summary = format!("{} 个窗口", figures.len());
        let report = self.build_word_report(figures, range_summary);
        match crate::word_export::write_word_report(&path, &report) {
            Ok(()) => {
                self.batch_export_last_summary = Some(self.tr("已导出 Word 报告。", "Exported Word report.").to_owned());
            }
            Err(error) => {
                self.batch_export_last_summary = Some(format!("Word 导出失败: {error}"));
            }
        }
    }
```

Extract `enabled_batch_export_windows` from the duplicated window validation in the PNG batch function.

- [ ] **Step 6: Add Word buttons**

Add a Word action to `export_preview_actions`:

```rust
                    if ui.button(self.tr("导出 Word", "Export Word")).clicked() {
                        self.save_export_preview_word();
                    }
```

Add a Word action to `batch_export_window`:

```rust
                    if ui
                        .button(self.tr("选择文件并导出 Word", "Choose File and Export Word"))
                        .clicked()
                    {
                        self.run_batch_waveform_word_export();
                    }
```

- [ ] **Step 7: Run compile check**

Run:

```bash
cargo check
```

Expected: Word export plumbing compiles.

## Task 9: Documentation And Help Text

**Files:**
- Modify: `README.md`
- Modify: `src/app.rs`

- [ ] **Step 1: Update README waveform export section**

Replace the waveform image export bullets with:

```markdown
### 波形图片与报告导出

- `Export > Export Waveform PNG` 打开截图软件式导出标注台，不会立即保存文件。
- 标注台左侧提供选择、文字、箭头、画笔、橡皮、撤销和重做工具。
- 变量名标注会自动生成，箭头默认吸附对应曲线；拖动变量名时箭头会自动变长、缩短和旋转，拖动锚点可改变箭头指向同一曲线的位置。
- 手动箭头和文字用于标注故障点、实验现象和说明，不绑定变量曲线。
- 可保存 PNG、SVG，也可导出 Word 报告。
- 批量导出可按多个时间窗口、数据组和子窗口生成多张 PNG，或把多张已标注波形图写入同一个 Word 报告。
- Word 报告使用内置简洁模板，光标数据表可选择包含或隐藏；第一版不导入外部 Word 模板。
```

- [ ] **Step 2: Update in-app help text**

Update the Chinese and English `Waveform Image Export` help strings around `src/app.rs` help rendering so they describe:

```rust
ui.label("导出标注台提供选择、文字、箭头、画笔、橡皮、撤销和重做。变量名箭头会自动吸附曲线，拖动变量名时箭头会跟随变化。");
ui.label("Word 报告会把一个或多个已标注波形图写入内置简洁模板，光标数据表可在导出设置中选择显示或隐藏。");
```

And English equivalents:

```rust
ui.label("The export workspace provides select, text, arrow, brush, eraser, undo, and redo tools. Variable arrows stay attached to curves and update as labels move.");
ui.label("Word report export writes one or more annotated waveform images into the built-in simple template. Cursor data tables can be shown or hidden from export settings.");
```

- [ ] **Step 3: Run docs grep check**

Run:

```bash
rg -n "Word report|Word 报告|外部 Word 模板|Variable arrows|变量名箭头" README.md src/app.rs
```

Expected: README and in-app help both mention the new Word report and variable arrow behavior.

## Task 10: Final Verification

**Files:**
- All touched files.

- [ ] **Step 1: Format**

Run:

```bash
cargo fmt
```

Expected: formatting completes without changing unrelated files.

- [ ] **Step 2: Run full tests**

Run:

```bash
cargo test
```

Expected: all tests pass. Real data tests may skip when sample files are absent.

- [ ] **Step 3: Run compile check**

Run:

```bash
cargo check
```

Expected: app compiles with new modules.

- [ ] **Step 4: Inspect changed files**

Run:

```bash
git diff --stat
git diff -- Cargo.toml src/main.rs src/export_annotation.rs src/png_export.rs src/word_export.rs src/app.rs README.md scripts/package-windows.ps1 scripts/ScopeAnalyzer.wxs
```

Expected: diff is scoped to export annotation, Word report, docs, and versioning.

- [ ] **Step 5: Manual UI smoke test**

Run the app:

```bash
cargo run
```

Manual checks:

- Open a waveform file.
- Select multiple curves.
- Open export annotation workspace.
- Drag a variable label and confirm the arrow follows while pointing to the curve.
- Drag the variable anchor and confirm it stays on the same curve.
- Add manual text and a manual arrow.
- Save PNG and confirm the file opens.
- Open batch export and export a Word report with cursor data tables enabled.
- Export another Word report with cursor data tables disabled.
- Open the `.docx` files in Word or a compatible viewer and confirm images, captions, and table toggle.

- [ ] **Step 6: Commit implementation**

If all verification passes:

```bash
git add Cargo.toml Cargo.lock src/main.rs src/export_annotation.rs src/png_export.rs src/word_export.rs src/app.rs README.md scripts/package-windows.ps1 scripts/ScopeAnalyzer.wxs
git commit -m "feat: add annotated waveform word export"
```
