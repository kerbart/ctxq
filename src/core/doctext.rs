//! Line splitting compatible with Python's `str.splitlines()`.
//!
//! Python splits on far more than `\n`: vertical tab, form feed,
//! file/group/record separators, NEL and the Unicode line and paragraph
//! separators. On ordinary markdown there is no difference; on a binary file
//! carrying the `.md` extension (the AppleDouble `._*` resource files macOS
//! scatters over a network share) the split changes, and with it the number
//! of chunks. Reproducing the rule guarantees an identical index.

/// Line breaks recognised by `str.splitlines()`, except `\r\n`, which is handled separately.
fn is_line_break(c: char) -> bool {
    matches!(
        c,
        '\n' | '\r'
            | '\u{0b}'
            | '\u{0c}'
            | '\u{1c}'
            | '\u{1d}'
            | '\u{1e}'
            | '\u{85}'
            | '\u{2028}'
            | '\u{2029}'
    )
}

/// Splits into lines without keeping the separators. A `\r\n` sequence
/// counts as a single break.
pub fn split_lines(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        if !is_line_break(c) {
            continue;
        }
        out.push(&text[start..i]);
        let mut next = i + c.len_utf8();
        if c == '\r' && chars.peek().is_some_and(|&(_, n)| n == '\n') {
            chars.next();
            next += 1;
        }
        start = next;
    }
    // No trailing empty line when the text ends with a separator.
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

/// Reads a file, dropping non-UTF-8 bytes instead of giving up.
///
/// The Python reference reads everything with `errors="ignore"`: a shell
/// script with a stray latin-1 byte keeps its description instead of going mute.
pub fn read_lossy(path: &std::path::Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    Some(match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => String::from_utf8_lossy(e.as_bytes()).replace('\u{FFFD}', ""),
    })
}
