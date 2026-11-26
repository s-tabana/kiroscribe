//! 資料のファイルから、題名と中身の文字を取り出す。ファイルは読むだけで、書き換えない。

use std::io::Read;
use std::path::Path;

use quick_xml::events::{BytesStart, Event};
use quick_xml::Reader;

/// 1 ファイルから読む文字の上限。
pub const MAX_CHARS: usize = 200_000;
/// 文書の中の題名として使う長さの上限（これより長ければ、ファイル名を題名にする）。
pub const MAX_TITLE_CHARS: usize = 60;

/// 索引に入れるファイルの種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Docx,
    Xlsx,
    Pptx,
    Pdf,
    Html,
    /// 名前だけで探す種類
    NameOnly,
}

/// 拡張子から種類を決める。索引に入れない種類なら None。
pub fn kind_of(path: &Path) -> Option<Kind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "txt" | "md" | "csv" | "tsv" => Kind::Text,
        "docx" => Kind::Docx,
        "xlsx" => Kind::Xlsx,
        "pptx" => Kind::Pptx,
        "pdf" => Kind::Pdf,
        "html" | "htm" => Kind::Html,
        "doc" | "xls" | "ppt" | "rtf" | "zip" | "png" | "jpg" | "jpeg" => Kind::NameOnly,
        _ => return None,
    })
}

/// 取り出した内容。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Extracted {
    /// 文書の中の題名（なければ None）
    pub title: Option<String>,
    pub body: String,
    /// 中身を読めず、名前だけで探す
    pub name_only: bool,
}

impl Extracted {
    fn name_only() -> Self {
        Self { title: None, body: String::new(), name_only: true }
    }
}

/// ファイルの中身を取り出す。読めないとき（壊れている・パスワード付き・文字がない）は、名前だけで探す。
pub fn extract(path: &Path, kind: Kind) -> Extracted {
    if kind == Kind::NameOnly {
        return Extracted::name_only();
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Extracted::name_only();
    };
    extract_bytes(&bytes, kind)
}

/// バイト列から取り出す（読み取りの部品が、壊れたファイルで止まらないようにする）。
pub fn extract_bytes(bytes: &[u8], kind: Kind) -> Extracted {
    let result = std::panic::catch_unwind(|| match kind {
        Kind::Text => Some((None, decode_text(bytes))),
        Kind::Html => {
            let html = decode_text(bytes);
            Some((html_title(&html), html_text(&html)))
        }
        Kind::Docx => docx(bytes),
        Kind::Xlsx => xlsx(bytes),
        Kind::Pptx => pptx(bytes),
        Kind::Pdf => pdf(bytes),
        Kind::NameOnly => None,
    });
    match result {
        Ok(Some((title, body))) if !body.trim().is_empty() => Extracted {
            title: title.map(|t| clean(&t)).filter(|t| !t.is_empty() && t.chars().count() <= MAX_TITLE_CHARS),
            body: truncate(body),
            name_only: false,
        },
        _ => Extracted::name_only(),
    }
}

fn truncate(text: String) -> String {
    match text.char_indices().nth(MAX_CHARS) {
        Some((end, _)) => text[..end].to_string(),
        None => text,
    }
}

/// 空白をまとめ、前後を除く。
fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// テキストの文字コードを見分けて読む（UTF-8〔BOM の有無を問わない〕、BOM 付きの UTF-16、Shift_JIS）。
pub fn decode_text(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(rest).into_owned();
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        return encoding_rs::UTF_16LE.decode_with_bom_removal(bytes).0.into_owned();
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        return encoding_rs::UTF_16BE.decode_with_bom_removal(bytes).0.into_owned();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => encoding_rs::SHIFT_JIS.decode_without_bom_handling(bytes).0.into_owned(),
    }
}

// ---- HTML ----

fn find_ci(haystack: &str, needle: &str, from: usize) -> Option<usize> {
    haystack.get(from..)?.to_ascii_lowercase().find(needle).map(|i| i + from)
}

fn html_title(html: &str) -> Option<String> {
    let start = find_ci(html, "<title", 0)?;
    let open_end = html[start..].find('>')? + start + 1;
    let end = find_ci(html, "</title", open_end)?;
    Some(decode_entities(&html[open_end..end]))
}

/// タグを除いた本文（script・style・title の中身は除く）。
fn html_text(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    while i < html.len() {
        let Some(lt) = html[i..].find('<').map(|p| p + i) else {
            out.push_str(&decode_entities(&html[i..]));
            break;
        };
        out.push_str(&decode_entities(&html[i..lt]));
        let skip_to = ["script", "style", "title"].iter().find_map(|tag| {
            lower[lt + 1..]
                .starts_with(tag)
                .then(|| lower[lt..].find(&format!("</{tag}")).map(|p| p + lt))
                .flatten()
        });
        let from = skip_to.unwrap_or(lt);
        match html[from..].find('>') {
            Some(gt) => {
                i = from + gt + 1;
                out.push(' ');
            }
            None => break,
        }
    }
    clean(&out)
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let resolved = after.find(';').filter(|&semi| semi <= 10).and_then(|semi| {
            let name = &after[..semi];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some(' '),
                _ => name.strip_prefix('#').and_then(|num| {
                    let code = match num.strip_prefix(['x', 'X']) {
                        Some(hex) => u32::from_str_radix(hex, 16).ok(),
                        None => num.parse().ok(),
                    };
                    code.and_then(char::from_u32)
                }),
            };
            c.map(|c| (c, semi))
        });
        match resolved {
            Some((c, semi)) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

// ---- Office の文書（zip の中の XML） ----

fn zip_entry(archive: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut file = archive.by_name(name).ok()?;
    let mut text = String::new();
    // 中身が大きすぎる（壊れた・細工した）ファイルで止まらないよう、読む量に上限を付ける
    file.by_ref().take(64 * 1024 * 1024).read_to_string(&mut text).ok()?;
    Some(text)
}

fn open_zip(bytes: &[u8]) -> Option<zip::ZipArchive<std::io::Cursor<&[u8]>>> {
    zip::ZipArchive::new(std::io::Cursor::new(bytes)).ok()
}

fn attr(e: &BytesStart, name: &str) -> Option<String> {
    e.try_get_attribute(name).ok().flatten().and_then(|a| a.normalized_value(quick_xml::XmlVersion::Implicit1_0).ok().map(|v| v.into_owned()))
}

/// XML を読み、イベントごとに `f` を呼ぶ。`f` には、要素の始まり（空の要素を含む）・文字・終わりを渡す。
enum Xml<'a> {
    Start(&'a BytesStart<'a>),
    Text(&'a str),
    End(&'a str),
}

fn walk_xml(xml: &str, mut f: impl FnMut(Xml)) -> Option<()> {
    let mut reader = Reader::from_str(xml);
    loop {
        match reader.read_event().ok()? {
            Event::Start(e) => f(Xml::Start(&e)),
            Event::Empty(e) => {
                f(Xml::Start(&e));
                f(Xml::End(e.name().as_ref()));
            }
            Event::Text(t) => f(Xml::Text(&t.xml10_content())),
            Event::CData(t) => f(Xml::Text(&t.into_inner())),
            Event::GeneralRef(r) => {
                let c = match r.resolve_char_ref().ok().flatten() {
                    Some(c) => Some(c),
                    None => match r.xml10_content().as_ref() {
                        "amp" => Some('&'),
                        "lt" => Some('<'),
                        "gt" => Some('>'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    },
                };
                if let Some(c) = c {
                    f(Xml::Text(c.encode_utf8(&mut [0; 4])));
                }
            }
            Event::End(e) => f(Xml::End(e.name().as_ref())),
            Event::Eof => return Some(()),
            _ => {}
        }
    }
}

/// Word: 本文と、最初の見出し（見出しの段落スタイルか、アウトラインのレベルがある段落）。
fn docx(bytes: &[u8]) -> Option<(Option<String>, String)> {
    let mut archive = open_zip(bytes)?;
    let xml = zip_entry(&mut archive, "word/document.xml")?;
    let mut body = String::new();
    let mut paragraph = String::new();
    let mut heading = false;
    let mut in_text = false;
    let mut title = None;
    walk_xml(&xml, |event| match event {
        Xml::Start(e) => match e.name().as_ref() {
            "w:p" => {
                paragraph.clear();
                heading = false;
            }
            "w:pStyle" => {
                let style = attr(e, "w:val").unwrap_or_default().to_ascii_lowercase();
                // 英語版は Heading1・Title、日本語版の組み込みの見出しは 1〜9
                if style.starts_with("heading") || style == "title" || (style.len() == 1 && style.chars().all(|c| c.is_ascii_digit())) {
                    heading = true;
                }
            }
            "w:outlineLvl" => heading = true,
            "w:t" => in_text = true,
            "w:tab" => paragraph.push('\t'),
            "w:br" => paragraph.push('\n'),
            _ => {}
        },
        Xml::Text(text) if in_text => paragraph.push_str(text),
        Xml::Text(_) => {}
        Xml::End(name) => match name {
            "w:t" => in_text = false,
            "w:p" => {
                if heading && title.is_none() && !paragraph.trim().is_empty() {
                    title = Some(paragraph.clone());
                }
                body.push_str(&paragraph);
                body.push('\n');
            }
            _ => {}
        },
    })?;
    Some((title, body))
}

/// zip の中の、`prefix` + 番号 + `.xml` の名前を、番号の順に並べる。
fn numbered_entries(archive: &zip::ZipArchive<std::io::Cursor<&[u8]>>, prefix: &str) -> Vec<String> {
    let mut names: Vec<(u32, String)> = archive
        .file_names()
        .filter_map(|name| {
            let name = name.ok()?;
            let number = name.strip_prefix(prefix)?.strip_suffix(".xml")?.parse().ok()?;
            Some((number, name.to_string()))
        })
        .collect();
    names.sort();
    names.into_iter().map(|(_, name)| name).collect()
}

/// `tag` の中の文字を、段落（`paragraph` の要素）ごとに改行して集める。
fn texts(xml: &str, tag: &str, paragraph: &str, out: &mut String) -> Option<()> {
    let mut inside = false;
    walk_xml(xml, |event| match event {
        Xml::Start(e) if e.name().as_ref() == tag => inside = true,
        Xml::Text(text) if inside => out.push_str(text),
        Xml::End(name) if name == tag => inside = false,
        Xml::End(name) if name == paragraph => out.push('\n'),
        _ => {}
    })
}

/// Excel: 共有の文字列と、シートの中の文字列（題名は持たない）。
fn xlsx(bytes: &[u8]) -> Option<(Option<String>, String)> {
    let mut archive = open_zip(bytes)?;
    let mut body = String::new();
    if let Some(xml) = zip_entry(&mut archive, "xl/sharedStrings.xml") {
        texts(&xml, "t", "si", &mut body)?;
    }
    for name in numbered_entries(&archive, "xl/worksheets/sheet") {
        if let Some(xml) = zip_entry(&mut archive, &name) {
            texts(&xml, "t", "is", &mut body)?;
        }
    }
    Some((None, body))
}

/// PowerPoint: すべてのスライドの文字と、1 枚目のタイトル。
fn pptx(bytes: &[u8]) -> Option<(Option<String>, String)> {
    let mut archive = open_zip(bytes)?;
    let slides = numbered_entries(&archive, "ppt/slides/slide");
    if slides.is_empty() {
        return None;
    }
    let mut body = String::new();
    let mut title = None;
    for (index, name) in slides.iter().enumerate() {
        let xml = zip_entry(&mut archive, name)?;
        let mut shape = String::new();
        let mut is_title = false;
        let mut in_text = false;
        walk_xml(&xml, |event| match event {
            Xml::Start(e) => match e.name().as_ref() {
                "p:sp" => {
                    shape.clear();
                    is_title = false;
                }
                "p:ph" => {
                    if matches!(attr(e, "type").as_deref(), Some("title" | "ctrTitle")) {
                        is_title = true;
                    }
                }
                "a:t" => in_text = true,
                _ => {}
            },
            Xml::Text(text) if in_text => {
                shape.push_str(text);
                body.push_str(text);
            }
            Xml::Text(_) => {}
            Xml::End(name) => match name {
                "a:t" => in_text = false,
                "a:p" => {
                    shape.push(' ');
                    body.push('\n');
                }
                "p:sp" if is_title && index == 0 && title.is_none() && !shape.trim().is_empty() => {
                    title = Some(shape.clone());
                }
                _ => {}
            },
        })?;
    }
    Some((title, body))
}

// ---- PDF ----

/// PDF の文字列（UTF-16BE〔BOM 付き〕か、それ以外は 1 バイト 1 文字）。
fn pdf_string(bytes: &[u8]) -> String {
    match bytes.strip_prefix(&[0xFE, 0xFF]) {
        Some(rest) => encoding_rs::UTF_16BE.decode_without_bom_handling(rest).0.into_owned(),
        None => bytes.iter().map(|&b| b as char).collect(),
    }
}

fn pdf_title(doc: &pdf_extract::Document) -> Option<String> {
    let info = doc.trailer.get(b"Info").ok()?;
    let info = match info {
        pdf_extract::Object::Reference(id) => doc.get_object(*id).ok()?,
        other => other,
    };
    match info.as_dict().ok()?.get(b"Title").ok()? {
        pdf_extract::Object::String(bytes, _) => Some(pdf_string(bytes)),
        _ => None,
    }
}

/// PDF: 文字と、文書のプロパティの題名。暗号化された（パスワード付きの）ものは読まない。
fn pdf(bytes: &[u8]) -> Option<(Option<String>, String)> {
    let doc = pdf_extract::Document::load_mem(bytes).ok()?;
    if doc.is_encrypted() {
        return None;
    }
    let title = pdf_title(&doc);
    let text = pdf_extract::extract_text_from_mem(bytes).ok()?;
    Some((title, text))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    /// テスト用の zip（Office の文書）を作る。
    pub fn zip_of(entries: &[(&str, &str)]) -> Vec<u8> {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            for (name, content) in entries {
                writer.start_file(*name, zip::write::SimpleFileOptions::default()).unwrap();
                writer.write_all(content.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        buffer.into_inner()
    }

    pub fn docx_of(title: &str, body: &str) -> Vec<u8> {
        let xml = format!(
            r#"<?xml version="1.0"?><w:document xmlns:w="w"><w:body>
            <w:p><w:r><w:t>表紙のことば</w:t></w:r></w:p>
            <w:p><w:pPr><w:pStyle w:val="1"/></w:pPr><w:r><w:t>{title}</w:t></w:r></w:p>
            <w:p><w:r><w:t xml:space="preserve">{body} &amp; </w:t></w:r><w:r><w:t>続き</w:t></w:r></w:p>
            </w:body></w:document>"#
        );
        zip_of(&[("word/document.xml", &xml)])
    }

    /// 文字を 1 行だけ持つ PDF（Helvetica、文書のプロパティの題名付き）。
    pub fn pdf_of(text: &str, title: &str) -> Vec<u8> {
        let stream = format!("BT /F1 12 Tf 72 720 Td ({text}) Tj ET");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            format!("<< /Title ({title}) >>"),
        ];
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!("trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes(),
        );
        out
    }

    // [IDX-004]
    #[test]
    fn kinds_by_extension() {
        let kind = |name: &str| kind_of(Path::new(name));
        for name in ["a.txt", "a.MD", "a.csv", "a.tsv"] {
            assert_eq!(kind(name), Some(Kind::Text), "{name}");
        }
        assert_eq!(kind("a.docx"), Some(Kind::Docx));
        assert_eq!(kind("a.xlsx"), Some(Kind::Xlsx));
        assert_eq!(kind("a.pptx"), Some(Kind::Pptx));
        assert_eq!(kind("a.pdf"), Some(Kind::Pdf));
        assert_eq!(kind("a.html"), Some(Kind::Html));
        assert_eq!(kind("a.HTM"), Some(Kind::Html));
        for name in ["a.doc", "a.xls", "a.ppt", "a.rtf", "a.zip", "a.png", "a.jpg", "a.jpeg"] {
            assert_eq!(kind(name), Some(Kind::NameOnly), "{name}");
        }
        for name in ["a.exe", "a.bat", "a.ps1", "a.js", "a.lnk", "a.dll", "noext"] {
            assert_eq!(kind(name), None, "{name}");
        }
        assert_eq!(extract_bytes(b"anything", Kind::NameOnly), Extracted::name_only());
    }

    // [IDX-005]
    #[test]
    fn text_encodings_are_detected() {
        let text = "経費精算の手順";
        assert_eq!(decode_text(text.as_bytes()), text, "UTF-8（BOM なし）");
        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(text.as_bytes());
        assert_eq!(decode_text(&bom), text, "UTF-8（BOM 付き）");
        let mut le = vec![0xFF, 0xFE];
        le.extend(text.encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert_eq!(decode_text(&le), text, "UTF-16LE");
        let mut be = vec![0xFE, 0xFF];
        be.extend(text.encode_utf16().flat_map(|u| u.to_be_bytes()));
        assert_eq!(decode_text(&be), text, "UTF-16BE");
        let (sjis, _, _) = encoding_rs::SHIFT_JIS.encode(text);
        assert_eq!(decode_text(&sjis), text, "Shift_JIS");
    }

    // [IDX-005]
    #[test]
    fn unreadable_files_are_searched_by_name_only() {
        assert!(extract_bytes(b"not a zip", Kind::Docx).name_only, "壊れた Word");
        assert!(extract_bytes(b"%PDF-1.4 broken", Kind::Pdf).name_only, "壊れた PDF");
        // 文字を含まない PDF（画像だけのものと同じく、取り出せる文字がない）
        assert!(extract_bytes(&pdf_of("", "t"), Kind::Pdf).name_only, "文字のない PDF");
        // パスワード付きの Office の文書は zip ではない（OLE の形式）
        assert!(extract_bytes(&[0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1], Kind::Xlsx).name_only);
        // 暗号化した PDF
        let encrypted = String::from_utf8_lossy(&pdf_of("secret", "t"))
            .replace("/Info 6 0 R", "/Info 6 0 R /Encrypt << /Filter /Standard /V 1 /R 2 /O (x) /U (y) /P -4 >>");
        assert!(extract_bytes(encrypted.as_bytes(), Kind::Pdf).name_only, "パスワード付きの PDF");
    }

    // [IDX-004][IDX-006]
    #[test]
    fn office_pdf_and_html_contents_and_titles() {
        let word = extract_bytes(&docx_of("経費精算のマニュアル", "申請は月末まで"), Kind::Docx);
        assert_eq!(word.title.as_deref(), Some("経費精算のマニュアル"), "Word の最初の見出し");
        assert!(word.body.contains("申請は月末まで & 続き"), "{}", word.body);
        assert!(!word.name_only);

        let sheet = zip_of(&[
            ("xl/sharedStrings.xml", r#"<sst><si><t>交通費</t></si><si><r><t>宿泊</t></r><r><t>費</t></r></si></sst>"#),
            ("xl/worksheets/sheet1.xml", r#"<worksheet><c t="inlineStr"><is><t>出張の上限</t></is></c></worksheet>"#),
        ]);
        let excel = extract_bytes(&sheet, Kind::Xlsx);
        assert_eq!(excel.title, None, "Excel は題名を持たない（ファイル名を使う）");
        assert!(excel.body.contains("交通費") && excel.body.contains("宿泊費") && excel.body.contains("出張の上限"), "{}", excel.body);

        let slide = |title: &str, text: &str| {
            format!(r#"<p:sld><p:sp><p:nvSpPr><p:nvPr><p:ph type="title"/></p:nvPr></p:nvSpPr><p:txBody><a:p><a:r><a:t>{title}</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:txBody><a:p><a:r><a:t>{text}</a:t></a:r></a:p></p:txBody></p:sp></p:sld>"#)
        };
        let deck = zip_of(&[
            ("ppt/slides/slide2.xml", &slide("2 枚目", "予算の内訳")),
            ("ppt/slides/slide1.xml", &slide("新製品の発表", "日程")),
        ]);
        let ppt = extract_bytes(&deck, Kind::Pptx);
        assert_eq!(ppt.title.as_deref(), Some("新製品の発表"), "1 枚目のタイトル");
        assert!(ppt.body.find("日程").unwrap() < ppt.body.find("予算の内訳").unwrap(), "スライドの順: {}", ppt.body);

        let pdf = extract_bytes(&pdf_of("Expense policy", "Travel Guide"), Kind::Pdf);
        assert_eq!(pdf.title.as_deref(), Some("Travel Guide"), "PDF の文書のプロパティの題名");
        assert!(pdf.body.contains("Expense policy"), "{}", pdf.body);

        let html = "<html><head><title>社内 Wiki &amp; 手順</title><style>p{}</style><script>var x=1</script></head><body><p>VPN の&nbsp;設定</p></body></html>";
        let page = extract_bytes(html.as_bytes(), Kind::Html);
        assert_eq!(page.title.as_deref(), Some("社内 Wiki & 手順"));
        assert_eq!(page.body, "VPN の 設定");

        // 60 文字を超える題名は使わない
        let long = "長".repeat(61);
        assert_eq!(extract_bytes(&docx_of(&long, "本文"), Kind::Docx).title, None);
        assert_eq!(extract_bytes(&docx_of(&"長".repeat(60), "本文"), Kind::Docx).title, Some("長".repeat(60)));
    }

    // [IDX-003]
    #[test]
    fn reads_at_most_200000_characters() {
        let text = "あ".repeat(MAX_CHARS + 10);
        let read = extract_bytes(text.as_bytes(), Kind::Text);
        assert_eq!(read.body.chars().count(), MAX_CHARS);
    }
}
