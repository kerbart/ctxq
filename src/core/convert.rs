//! Office documents and PDF → markdown, in-process (no MarkItDown, no network).
//!
//! The output is markdown on purpose: it then goes through the same chunker as
//! any `.md` file (sections by heading, tables row by row for spreadsheets), so
//! a slide deck, a contract or a sheet is searchable like the rest.
//!
//! OOXML files are zip archives of XML parts; only the text-bearing parts are
//! read. Conversion is best effort: formatting is dropped, structure (headings,
//! tables, slides, sheets, speaker notes) is kept.

use std::collections::BTreeMap;
use std::io::Read;

use anyhow::{bail, Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;

/// Extensions this module converts.
pub const CONVERTIBLE: &[&str] = &["docx", "pptx", "xlsx", "pdf"];

/// Rows kept per spreadsheet sheet: a 100 k-row export is data, not knowledge.
const MAX_SHEET_ROWS: usize = 2000;

pub fn is_convertible(ext: &str) -> bool {
    CONVERTIBLE.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// Converts `bytes` (a file with extension `ext`) to markdown titled `title`.
pub fn to_markdown(ext: &str, bytes: &[u8], title: &str) -> Result<String> {
    let body = match ext.to_ascii_lowercase().as_str() {
        "docx" => docx(bytes)?,
        "pptx" => pptx(bytes)?,
        "xlsx" => xlsx(bytes)?,
        "pdf" => pdf(bytes)?,
        other => bail!("no converter for .{other}"),
    };
    // A leading H1 unless the document brings its own.
    if body.trim_start().starts_with("# ") {
        Ok(body)
    } else {
        Ok(format!("# {title}\n\n{body}"))
    }
}

fn zip_part(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, name: &str) -> Option<String> {
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn open_zip(bytes: &[u8]) -> Result<zip::ZipArchive<std::io::Cursor<&[u8]>>> {
    zip::ZipArchive::new(std::io::Cursor::new(bytes)).context("not a valid OOXML (zip) file")
}

fn attr(e: &quick_xml::events::BytesStart<'_>, local: &[u8]) -> Option<String> {
    e.attributes().flatten().find_map(|a| {
        (a.key.local_name().as_ref() == local)
            .then(|| String::from_utf8_lossy(&a.value).into_owned())
    })
}

fn md_table(rows: &[Vec<String>]) -> String {
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    if width == 0 {
        return String::new();
    }
    let esc = |c: &str| c.replace('|', "\\|").replace('\n', " ").trim().to_string();
    let line = |r: &Vec<String>| {
        let cells: Vec<String> = (0..width)
            .map(|i| esc(r.get(i).map_or("", String::as_str)))
            .collect();
        format!("| {} |", cells.join(" | "))
    };
    let mut out = vec![line(&rows[0]), format!("|{}", "---|".repeat(width))];
    out.extend(rows[1..].iter().map(line));
    out.join("\n")
}

// ------------------------------------------------------------------ docx

/// Heading level of a paragraph style (`Heading2`, `Titre2`, `Title`…).
fn heading_level(style: &str) -> Option<usize> {
    let s = style.to_ascii_lowercase();
    if s == "title" || s == "titre" {
        return Some(1);
    }
    for prefix in ["heading", "titre", "berschrift", "kop"] {
        if let Some(n) = s
            .strip_prefix(prefix)
            .and_then(|r| r.trim().parse::<usize>().ok())
        {
            return Some((n + 1).clamp(2, 6));
        }
    }
    None
}

fn docx(bytes: &[u8]) -> Result<String> {
    let mut zip = open_zip(bytes)?;
    let xml = zip_part(&mut zip, "word/document.xml").context("word/document.xml missing")?;
    let mut reader = Reader::from_str(&xml);
    let mut out: Vec<String> = Vec::new();
    let (mut para, mut style) = (String::new(), None::<usize>);
    let mut table: Vec<Vec<String>> = Vec::new();
    let (mut row, mut cell) = (Vec::<String>::new(), String::new());
    let mut table_depth = 0usize;
    let mut in_text = false;
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"pStyle" => {
                style = attr(&e, b"val").as_deref().and_then(heading_level);
            }
            Event::Start(e) => match e.local_name().as_ref() {
                b"t" => in_text = true,
                b"tbl" => table_depth += 1,
                b"tr" if table_depth == 1 => row.clear(),
                b"tc" if table_depth == 1 => cell.clear(),
                _ => {}
            },
            Event::Empty(e) => match e.local_name().as_ref() {
                b"tab" => para.push('\t'),
                b"br" => para.push('\n'),
                _ => {}
            },
            Event::Text(t) if in_text => para.push_str(&t.unescape()?),
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" => {
                    let text = std::mem::take(&mut para);
                    let text = text.trim();
                    if table_depth > 0 {
                        if !text.is_empty() {
                            if !cell.is_empty() {
                                cell.push(' ');
                            }
                            cell.push_str(text);
                        }
                    } else if !text.is_empty() {
                        out.push(match style {
                            Some(n) => format!("{} {text}", "#".repeat(n)),
                            None => text.to_string(),
                        });
                    }
                    style = None;
                }
                b"tc" if table_depth == 1 => row.push(std::mem::take(&mut cell)),
                b"tr" if table_depth == 1 => table.push(std::mem::take(&mut row)),
                b"tbl" => {
                    table_depth = table_depth.saturating_sub(1);
                    if table_depth == 0 && !table.is_empty() {
                        out.push(md_table(&std::mem::take(&mut table)));
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out.join("\n\n"))
}

// ------------------------------------------------------------------ pptx

/// Text paragraphs of a slide-like part (`<a:p>` / `<a:t>`).
fn drawing_paragraphs(xml: &str) -> Result<Vec<String>> {
    let mut reader = Reader::from_str(xml);
    let (mut out, mut para, mut in_text) = (Vec::new(), String::new(), false);
    loop {
        match reader.read_event()? {
            Event::Start(e) if e.local_name().as_ref() == b"t" => in_text = true,
            Event::Text(t) if in_text => para.push_str(&t.unescape()?),
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" => {
                    let p = std::mem::take(&mut para);
                    if !p.trim().is_empty() {
                        out.push(p.trim().to_string());
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

fn numbered_parts(
    zip: &zip::ZipArchive<std::io::Cursor<&[u8]>>,
    prefix: &str,
) -> Vec<(u32, String)> {
    let mut parts: Vec<(u32, String)> = zip
        .file_names()
        .filter_map(|n| {
            let num = n.strip_prefix(prefix)?.strip_suffix(".xml")?.parse().ok()?;
            Some((num, n.to_string()))
        })
        .collect();
    parts.sort();
    parts
}

fn pptx(bytes: &[u8]) -> Result<String> {
    let mut zip = open_zip(bytes)?;
    let slides = numbered_parts(&zip, "ppt/slides/slide");
    if slides.is_empty() {
        bail!("no slide in this presentation");
    }
    let mut out = Vec::new();
    for (n, part) in slides {
        let xml = zip_part(&mut zip, &part).unwrap_or_default();
        let paras = drawing_paragraphs(&xml)?;
        let title = paras.first().cloned().unwrap_or_default();
        let mut section = format!("## Slide {n} — {title}");
        for p in paras.iter().skip(1) {
            section.push_str(&format!("\n\n{p}"));
        }
        // Speaker notes often hold what the slide only hints at.
        if let Some(notes) = zip_part(&mut zip, &format!("ppt/notesSlides/notesSlide{n}.xml")) {
            let notes: Vec<String> = drawing_paragraphs(&notes)?
                .into_iter()
                .filter(|p| p.parse::<u32>().is_err()) // slide-number placeholder
                .collect();
            if !notes.is_empty() {
                section.push_str(&format!("\n\n### Notes\n\n{}", notes.join("\n\n")));
            }
        }
        out.push(section);
    }
    Ok(out.join("\n\n"))
}

// ------------------------------------------------------------------ xlsx

fn shared_strings(xml: &str) -> Result<Vec<String>> {
    let mut reader = Reader::from_str(xml);
    let (mut out, mut cur, mut in_si, mut in_t) = (Vec::new(), String::new(), false, false);
    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"si" => {
                    in_si = true;
                    cur.clear();
                }
                b"t" => in_t = true,
                _ => {}
            },
            Event::Text(t) if in_si && in_t => cur.push_str(&t.unescape()?),
            Event::End(e) => match e.local_name().as_ref() {
                b"t" => in_t = false,
                b"si" => {
                    in_si = false;
                    out.push(std::mem::take(&mut cur));
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// `B12` → column index 1.
fn column_of(cell_ref: &str) -> usize {
    cell_ref
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .fold(0usize, |acc, b| {
            acc * 26 + (b.to_ascii_uppercase() - b'A' + 1) as usize
        })
        .saturating_sub(1)
}

#[allow(clippy::collapsible_match)]
fn sheet_rows(xml: &str, strings: &[String]) -> Result<Vec<Vec<String>>> {
    let mut reader = Reader::from_str(xml);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: BTreeMap<usize, String> = BTreeMap::new();
    let (mut col, mut kind, mut val, mut in_v) = (0usize, String::new(), String::new(), false);
    loop {
        match reader.read_event()? {
            Event::Start(e) => match e.local_name().as_ref() {
                b"row" => row.clear(),
                b"c" => {
                    col = attr(&e, b"r").map_or(row.len(), |r| column_of(&r));
                    kind = attr(&e, b"t").unwrap_or_default();
                    val.clear();
                }
                b"v" | b"t" => in_v = true,
                _ => {}
            },
            Event::Text(t) if in_v => val.push_str(&t.unescape()?),
            Event::End(e) => match e.local_name().as_ref() {
                b"v" | b"t" => in_v = false,
                b"c" => {
                    let v = if kind == "s" {
                        val.trim()
                            .parse::<usize>()
                            .ok()
                            .and_then(|i| strings.get(i))
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        val.clone()
                    };
                    if !v.trim().is_empty() {
                        row.insert(col, v);
                    }
                }
                b"row" => {
                    if !row.is_empty() {
                        let width = row.keys().max().map_or(0, |m| m + 1);
                        rows.push(
                            (0..width)
                                .map(|i| row.get(&i).cloned().unwrap_or_default())
                                .collect(),
                        );
                        if rows.len() >= MAX_SHEET_ROWS {
                            break;
                        }
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(rows)
}

fn xlsx(bytes: &[u8]) -> Result<String> {
    let mut zip = open_zip(bytes)?;
    let strings = zip_part(&mut zip, "xl/sharedStrings.xml")
        .map(|x| shared_strings(&x))
        .transpose()?
        .unwrap_or_default();
    // Sheet names, in workbook order, mapped to their parts through the rels.
    let workbook = zip_part(&mut zip, "xl/workbook.xml").context("xl/workbook.xml missing")?;
    let rels = zip_part(&mut zip, "xl/_rels/workbook.xml.rels").unwrap_or_default();
    let mut targets: BTreeMap<String, String> = BTreeMap::new();
    let mut reader = Reader::from_str(&rels);
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"Relationship" => {
                if let (Some(id), Some(t)) = (attr(&e, b"Id"), attr(&e, b"Target")) {
                    let t = t
                        .trim_start_matches('/')
                        .trim_start_matches("xl/")
                        .to_string();
                    targets.insert(id, format!("xl/{t}"));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut sheets = Vec::new();
    let mut reader = Reader::from_str(&workbook);
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) if e.local_name().as_ref() == b"sheet" => {
                let name =
                    attr(&e, b"name").unwrap_or_else(|| format!("Sheet{}", sheets.len() + 1));
                if let Some(part) = attr(&e, b"id").and_then(|id| targets.get(&id).cloned()) {
                    sheets.push((name, part));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut out = Vec::new();
    for (name, part) in sheets {
        let Some(xml) = zip_part(&mut zip, &part) else {
            continue;
        };
        let rows = sheet_rows(&xml, &strings)?;
        if rows.is_empty() {
            continue;
        }
        out.push(format!("## {name}\n\n{}", md_table(&rows)));
    }
    Ok(out.join("\n\n"))
}

// ------------------------------------------------------------------ pdf

/// Hidden sub-command running [`pdf_text_child`].
pub const PDF_CHILD_ARG: &str = "__pdf-text";

#[cfg(feature = "pdf")]
fn pdf(bytes: &[u8]) -> Result<String> {
    // pdf-extract prints diagnostics with `println!`, which would corrupt the
    // MCP stdio stream, and may panic or loop on malformed files. Extraction
    // therefore runs in a child process of this very binary, with a timeout.
    // Unit tests (where the current executable is the test harness) and
    // embedders of the library can opt into in-process extraction.
    if cfg!(test) || std::env::var_os("DOCUMENTUM_PDF_INPROCESS").is_some() {
        return pdf_in_process(bytes);
    }
    let dir = tempfile_dir()?;
    let (input, output) = (dir.join("in.pdf"), dir.join("out.md"));
    std::fs::write(&input, bytes)?;
    let exe = std::env::current_exe().context("locating the documentum binary")?;
    let argv = vec![
        exe.to_string_lossy().into_owned(),
        PDF_CHILD_ARG.to_string(),
        input.to_string_lossy().into_owned(),
        output.to_string_lossy().into_owned(),
    ];
    let res = crate::core::sources::run_command(&argv, &dir, std::time::Duration::from_secs(120))
        .and_then(|_| std::fs::read_to_string(&output).context("reading the extracted text"));
    let _ = std::fs::remove_dir_all(&dir);
    res
}

#[cfg(feature = "pdf")]
fn tempfile_dir() -> Result<std::path::PathBuf> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "documentum-pdf-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Child side: stdout is pointed at stderr first, so the library's chatter
/// never reaches whoever reads our stdout; the text goes to `output`.
#[cfg(feature = "pdf")]
pub fn pdf_text_child(input: &std::path::Path, output: &std::path::Path) -> Result<()> {
    // SAFETY: plain fd duplication in a single-purpose child process.
    unsafe {
        libc::dup2(2, 1);
    }
    let bytes = std::fs::read(input)?;
    std::fs::write(output, pdf_in_process(&bytes)?)?;
    Ok(())
}

#[cfg(feature = "pdf")]
fn pdf_in_process(bytes: &[u8]) -> Result<String> {
    let text = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(bytes))
        .map_err(|_| anyhow::anyhow!("PDF parser crashed on this file"))?
        .context("extracting PDF text")?;
    // Collapse the layout's hard wraps into paragraphs.
    let paras: Vec<String> = text
        .split("\n\n")
        .map(|p| p.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|p| !p.is_empty())
        .collect();
    if paras.is_empty() {
        bail!("no extractable text (scanned PDF? OCR is not done here)");
    }
    Ok(paras.join("\n\n"))
}

#[cfg(not(feature = "pdf"))]
fn pdf(_bytes: &[u8]) -> Result<String> {
    bail!("this binary was built without the 'pdf' feature")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn ooxml(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut z = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, body) in parts {
                z.start_file(*name, opts).unwrap();
                z.write_all(body.as_bytes()).unwrap();
            }
            z.finish().unwrap();
        }
        buf.into_inner()
    }

    #[test]
    fn docx_keeps_headings_paragraphs_and_tables() {
        let doc = r#"<w:document xmlns:w="w"><w:body>
            <w:p><w:pPr><w:pStyle w:val="Title"/></w:pPr><w:r><w:t>Contrat</w:t></w:r></w:p>
            <w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Objet</w:t></w:r></w:p>
            <w:p><w:r><w:t>Le prestataire </w:t></w:r><w:r><w:t>fournit &amp; maintient.</w:t></w:r></w:p>
            <w:tbl><w:tr><w:tc><w:p><w:r><w:t>Lot</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Prix</w:t></w:r></w:p></w:tc></w:tr>
            <w:tr><w:tc><w:p><w:r><w:t>A</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>10 €</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
            </w:body></w:document>"#;
        let md = to_markdown("docx", &ooxml(&[("word/document.xml", doc)]), "x").unwrap();
        assert!(
            md.starts_with("# Contrat\n\n## Objet\n\nLe prestataire fournit & maintient."),
            "{md}"
        );
        assert!(
            md.contains("| Lot | Prix |\n|---|---|\n| A | 10 € |"),
            "{md}"
        );
    }

    #[test]
    fn pptx_has_one_section_per_slide_with_notes() {
        let slide = |t: &str, b: &str| {
            format!(
                r#"<p:sld xmlns:a="a" xmlns:p="p"><a:p><a:r><a:t>{t}</a:t></a:r></a:p><a:p><a:r><a:t>{b}</a:t></a:r></a:p></p:sld>"#
            )
        };
        let (s1, s2) = (slide("Kickoff", "Objectifs"), slide("Planning", "Q3"));
        let notes = r#"<p:notes xmlns:a="a" xmlns:p="p"><a:p><a:r><a:t>Insister sur le budget</a:t></a:r></a:p><a:p><a:r><a:t>1</a:t></a:r></a:p></p:notes>"#;
        let bytes = ooxml(&[
            ("ppt/slides/slide2.xml", &s2),
            ("ppt/slides/slide1.xml", &s1),
            ("ppt/notesSlides/notesSlide1.xml", notes),
        ]);
        let md = to_markdown("pptx", &bytes, "Deck").unwrap();
        assert!(md.starts_with("# Deck\n\n## Slide 1 — Kickoff\n\nObjectifs\n\n### Notes\n\nInsister sur le budget"), "{md}");
        assert!(
            md.contains("## Slide 2 — Planning\n\nQ3") && !md.contains("\n\n1\n"),
            "{md}"
        );
    }

    #[test]
    fn xlsx_renders_each_sheet_as_a_table() {
        let wb = r#"<workbook xmlns:r="r"><sheets><sheet name="Ports" sheetId="1" r:id="rId1"/></sheets></workbook>"#;
        let rels = r#"<Relationships><Relationship Id="rId1" Target="worksheets/sheet1.xml"/></Relationships>"#;
        let ss = r#"<sst><si><t>Service</t></si><si><t>Port</t></si><si><t>alpha</t></si></sst>"#;
        let sheet = r#"<worksheet><sheetData>
            <row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
            <row r="2"><c r="A2" t="s"><v>2</v></c><c r="C2"><v>3003</v></c></row>
            </sheetData></worksheet>"#;
        let bytes = ooxml(&[
            ("xl/workbook.xml", wb),
            ("xl/_rels/workbook.xml.rels", rels),
            ("xl/sharedStrings.xml", ss),
            ("xl/worksheets/sheet1.xml", sheet),
        ]);
        let md = to_markdown("xlsx", &bytes, "Inventaire").unwrap();
        assert!(
            md.contains("## Ports\n\n| Service | Port |  |\n|---|---|---|\n| alpha |  | 3003 |"),
            "{md}"
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(to_markdown("docx", b"not a zip", "x").is_err());
        assert!(to_markdown("pdf", b"%PDF-garbage", "x").is_err());
    }
}
