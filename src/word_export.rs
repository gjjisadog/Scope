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
    zip.add_file("word/_rels/document.xml.rels", document_rels(report).as_bytes())?;
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
        &format!("实验名称：{}", report.experiment_name),
        false,
    ));
    body.push_str(&paragraph(&format!("数据文件：{}", report.source_name), false));
    body.push_str(&paragraph(&format!("导出时间：{}", report.exported_at), false));
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
