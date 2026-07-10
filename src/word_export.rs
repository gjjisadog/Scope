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
    #[error("DOCX has no figures")]
    EmptyReport,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub fn write_word_report(path: &Path, report: &WordReport) -> Result<(), WordExportError> {
    let file = File::create(path)?;
    write_word_report_to_writer(file, report)
}

#[allow(dead_code)]
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
    zip.add_file(
        "word/_rels/document.xml.rels",
        document_rels(report).as_bytes(),
    )?;
    zip.add_file("word/document.xml", document_xml(report).as_bytes())?;

    for (index, figure) in report.figures.iter().enumerate() {
        zip.add_file(&format!("word/media/image{}.png", index + 1), &figure.png)?;
    }

    zip.finish()?;
    Ok(())
}

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
    body.push_str(&paragraph(
        &format!("名称：{}", report.experiment_name),
        false,
    ));
    body.push_str(&paragraph(
        &format!("数据文件：{}", report.source_name),
        false,
    ));
    body.push_str(&paragraph(
        &format!("导出时间：{}", report.exported_at),
        false,
    ));
    if let Some(sample_rate) = &report.sample_rate {
        body.push_str(&paragraph(&format!("采样率：{sample_rate}"), false));
    }
    body.push_str(&paragraph(
        &format!("时间范围：{}", report.time_range_summary),
        false,
    ));

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
    format!(
        r#"<w:tc><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:tc>"#,
        escape_xml(text)
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ]
    }

    fn le_u16(bytes: &[u8], offset: usize) -> u16 {
        u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
    }

    fn le_u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
    }

    fn zip_entry(bytes: &[u8], name: &str) -> Vec<u8> {
        let mut offset = 0;
        while offset + 30 <= bytes.len() {
            if le_u32(bytes, offset) != 0x0403_4b50 {
                break;
            }
            let method = le_u16(bytes, offset + 8);
            let size = le_u32(bytes, offset + 18) as usize;
            let name_len = le_u16(bytes, offset + 26) as usize;
            let extra_len = le_u16(bytes, offset + 28) as usize;
            let name_start = offset + 30;
            let data_start = name_start + name_len + extra_len;
            let data_end = data_start + size;
            assert!(data_end <= bytes.len(), "zip entry extends beyond archive");
            let entry_name = std::str::from_utf8(&bytes[name_start..name_start + name_len])
                .expect("zip entry names are utf-8");
            if entry_name == name {
                assert_eq!(method, 0, "test extractor only supports stored entries");
                return bytes[data_start..data_end].to_vec();
            }
            offset = data_end;
        }
        panic!("zip entry not found: {name}");
    }

    fn zip_entry_text(bytes: &[u8], name: &str) -> String {
        String::from_utf8(zip_entry(bytes, name)).expect("xml entry should be utf-8")
    }

    fn libreoffice_executable() -> Option<std::path::PathBuf> {
        if let Some(path) = std::env::var_os("SCOPE_LIBREOFFICE_PATH") {
            let path = std::path::PathBuf::from(path);
            if path.is_file() {
                return Some(path);
            }
        }
        let path_var = std::env::var_os("PATH")?;
        for dir in std::env::split_paths(&path_var) {
            for name in ["soffice.exe", "soffice", "libreoffice.exe", "libreoffice"] {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        None
    }

    fn zip_contains_file(bytes: &[u8], name: &str) -> bool {
        bytes
            .windows(name.len())
            .any(|window| window == name.as_bytes())
    }

    fn sample_report(include_cursor_tables: bool) -> WordReport {
        WordReport {
            title: "波形导出".to_owned(),
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

    #[test]
    fn word_report_test_fixture_uses_a_structurally_valid_png() {
        let png = sample_png();

        assert!(png.starts_with(b"\x89PNG\r\n\x1A\n"));
        assert_eq!(&png[12..16], b"IHDR");
        assert_eq!(&png[png.len() - 8..png.len() - 4], b"IEND");
    }

    #[test]
    fn word_report_preserves_chinese_and_escapes_xml_special_characters() {
        let mut report = sample_report(true);
        report.title = "波形导出 & 验证 <DOCX>".to_owned();
        report.experiment_name = "实验 \"A\" & 'B'".to_owned();
        report.source_name = "电流<采样>&电压.csv".to_owned();
        report.figures[0].caption = "图 1：A<B & C>D \"quote\" 'apostrophe'".to_owned();
        report.figures[0].cursor_table = Some(CursorTable {
            headers: vec!["变量&名称".to_owned(), "Y<X1>".to_owned()],
            rows: vec![vec!["Ia\"相\"".to_owned(), "1 < 2 & 3 > 2".to_owned()]],
        });

        let bytes = write_word_report_to_vec(&report).unwrap();
        let document = zip_entry_text(&bytes, "word/document.xml");

        assert!(document.contains("波形导出"));
        assert!(document.contains("验证"));
        assert!(document.contains("&amp;"));
        assert!(document.contains("&lt;DOCX&gt;"));
        assert!(document.contains("&quot;A&quot;"));
        assert!(document.contains("&apos;B&apos;"));
        assert!(document.contains("变量&amp;名称"));
        assert!(document.contains("1 &lt; 2 &amp; 3 &gt; 2"));
    }

    #[test]
    fn word_report_writes_multiple_images_relationships_and_media_parts() {
        let mut report = sample_report(false);
        report.figures.push(WordReportFigure {
            caption: "图 2：large".to_owned(),
            png: sample_png(),
            width_px: 12_000,
            height_px: 4_000,
            cursor_table: None,
        });
        report.figures.push(WordReportFigure {
            caption: "图 3：tall".to_owned(),
            png: sample_png(),
            width_px: 800,
            height_px: 2_400,
            cursor_table: None,
        });

        let bytes = write_word_report_to_vec(&report).unwrap();
        let content_types = zip_entry_text(&bytes, "[Content_Types].xml");
        let rels = zip_entry_text(&bytes, "word/_rels/document.xml.rels");
        let document = zip_entry_text(&bytes, "word/document.xml");

        for index in 1..=3 {
            assert!(zip_contains_file(
                &bytes,
                &format!("word/media/image{index}.png")
            ));
            assert!(content_types.contains(&format!("/word/media/image{index}.png")));
            assert!(rels.contains(&format!(r#"Id="rId{index}""#)));
            assert!(rels.contains(&format!(r#"Target="media/image{index}.png""#)));
            assert!(document.contains(&format!(r#"r:embed="rId{index}""#)));
        }
    }

    #[test]
    fn word_report_scales_large_images_to_page_width_preserving_aspect_ratio() {
        let (wide_cx, wide_cy) = image_size_emu(12_000, 4_000);
        let (tall_cx, tall_cy) = image_size_emu(800, 2_400);

        assert_eq!(wide_cx, (6.5 * EMU_PER_INCH as f64).round() as i64);
        assert_eq!(wide_cy, (wide_cx as f64 / 3.0).round() as i64);
        assert_eq!(tall_cx, wide_cx);
        assert_eq!(tall_cy, (wide_cx as f64 * 3.0).round() as i64);
    }

    #[test]
    #[ignore = "requires LibreOffice/soffice; set SCOPE_LIBREOFFICE_PATH or PATH"]
    fn word_report_can_be_converted_by_libreoffice_when_available() {
        let Some(soffice) = libreoffice_executable() else {
            eprintln!("LibreOffice/soffice was not found; skipping compatibility smoke test");
            return;
        };
        let dir = std::env::temp_dir().join(format!("scope_word_export_lo_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let docx = dir.join("compatibility.docx");
        let pdf = dir.join("compatibility.pdf");
        write_word_report(&docx, &sample_report(true)).unwrap();

        let status = std::process::Command::new(soffice)
            .arg("--headless")
            .arg("--convert-to")
            .arg("pdf")
            .arg("--outdir")
            .arg(&dir)
            .arg(&docx)
            .status()
            .unwrap();

        assert!(status.success(), "LibreOffice conversion failed: {status}");
        assert!(pdf.is_file(), "LibreOffice did not create {pdf:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
