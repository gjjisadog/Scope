use std::{fs, io::Cursor, path::Path};

use encoding_rs::{GBK, WINDOWS_1252};

use super::{DataError, DataResult};

const MOJIBAKE_CHARS: &[char] = &[
    '�', 'Ã', 'Â', 'Ä', 'Å', 'Æ', 'Ç', 'È', 'É', 'Ê', 'Ë', 'Ì', 'Í', 'Î', 'Ï', 'Ð', 'Ñ', 'Ò', 'Ó',
    'Ô', 'Õ', 'Ö', '×', 'Ø', 'Ù', 'Ú', 'Û', 'Ü', 'Ý', 'Þ', 'ß', 'à', 'á', 'â', 'ã', 'ä', 'å', 'æ',
    'ç', 'è', 'é', 'ê', 'ë', 'ì', 'í', 'î', 'ï', 'ð', 'ñ', 'ò', 'ó', 'ô', 'õ', '÷', 'ø', 'ù', 'ú',
    'û', 'ü', 'ý', 'þ', 'ÿ', '鏃', '堕', '棿', '鐢', '綉', '鍘', '绾', '妯', '姣', '杈', 'ュ',
    'ぇ', '傛', '哄', '垪', '瀛', '楁', '枃', '欢', '诲', '啓', '閿', '欒', '囦', '绌', '搴', '殑',
    '冧', '拰', '斤', '拷',
];

pub fn csv_reader_from_path(path: &Path) -> DataResult<csv::Reader<Cursor<Vec<u8>>>> {
    csv_reader_from_path_with_headers(path, false)
}

pub fn csv_reader_from_path_with_headers(
    path: &Path,
    has_headers: bool,
) -> DataResult<csv::Reader<Cursor<Vec<u8>>>> {
    let text = read_text_file(path)?;
    Ok(csv::ReaderBuilder::new()
        .flexible(true)
        .has_headers(has_headers)
        .trim(csv::Trim::All)
        .from_reader(Cursor::new(text.into_bytes())))
}

pub fn read_text_file(path: &Path) -> DataResult<String> {
    let bytes = fs::read(path)?;
    if let Ok(text) = std::str::from_utf8(&bytes) {
        return Ok(strip_utf8_bom(text).to_owned());
    }

    let (text, _, had_errors) = GBK.decode(&bytes);
    if had_errors {
        return Err(DataError::Csv(format!(
            "CSV text encoding is not UTF-8 or GBK: {}",
            path.display()
        )));
    }
    Ok(strip_utf8_bom(&text).to_owned())
}

pub fn decode_label_bytes(raw: &[u8]) -> String {
    let raw = trim_label_bytes(raw);
    if raw.is_empty() {
        return String::new();
    }

    if let Ok(name) = std::str::from_utf8(raw) {
        return normalize_label(name);
    }

    let (name, _, _) = GBK.decode(raw);
    let name = normalize_label(&name);
    if !name.is_empty() {
        return name;
    }

    normalize_label(&String::from_utf8_lossy(raw))
}

pub fn normalize_label(name: &str) -> String {
    let cleaned = clean_label(name);
    if cleaned.is_empty() {
        return String::new();
    }

    let repaired = repair_mojibake(cleaned);
    if score_mojibake(&repaired) < score_mojibake(cleaned) {
        repaired
    } else {
        cleaned.to_owned()
    }
}

fn trim_label_bytes(raw: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = raw.len();
    while start < end && is_label_padding(raw[start]) {
        start += 1;
    }
    while end > start && is_label_padding(raw[end - 1]) {
        end -= 1;
    }
    &raw[start..end]
}

fn is_label_padding(byte: u8) -> bool {
    byte.is_ascii_whitespace() || byte == 0x00 || byte == 0xff
}

fn clean_label(name: &str) -> &str {
    name.trim_matches(|ch: char| ch.is_whitespace() || ch == '\0' || ch == '\u{feff}')
}

fn strip_utf8_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn score_mojibake(text: &str) -> usize {
    text.chars()
        .filter(|ch| MOJIBAKE_CHARS.contains(ch))
        .count()
}

fn repair_mojibake(text: &str) -> String {
    repair_utf8_as_gbk(text)
        .or_else(|| repair_windows_1252_as_gbk(text))
        .unwrap_or_else(|| text.to_owned())
}

fn repair_utf8_as_gbk(text: &str) -> Option<String> {
    let (bytes, _, had_errors) = GBK.encode(text);
    if had_errors {
        return None;
    }
    let repaired = std::str::from_utf8(&bytes).ok()?;
    Some(clean_label(repaired).to_owned())
}

fn repair_windows_1252_as_gbk(text: &str) -> Option<String> {
    let (bytes, _, had_errors) = WINDOWS_1252.encode(text);
    if had_errors {
        return None;
    }
    let (repaired, _, had_errors) = GBK.decode(&bytes);
    if had_errors {
        return None;
    }
    Some(clean_label(&repaired).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_gbk_label_bytes() {
        let (bytes, _, _) = GBK.encode("电网电压");
        assert_eq!(decode_label_bytes(&bytes), "电网电压");
    }

    #[test]
    fn repairs_utf8_misdecoded_as_gbk_label() {
        assert_eq!(normalize_label("鏃堕棿"), "时间");
    }

    #[test]
    fn repairs_gbk_misdecoded_as_windows_1252_label() {
        assert_eq!(normalize_label("ÖÐÎÄ"), "中文");
    }
}
