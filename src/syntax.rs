//! Syntax colors for a few file types. The edit control can only draw in one
//! color, so it paints the text as usual and the colored runs are drawn again
//! on top, at the positions the control reports for each character.

use crate::{muldiv, theme};
use windows_sys::Win32::Foundation::{HWND, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

/// Every edit lexes the whole text (block comments can open anywhere above),
/// so bigger files show plain.
const MAX_CHARS: i32 = 1 << 20;

/// The text last painted and all of its runs. Scrolling repaints on every
/// wheel notch; lexing again only when the text changed keeps that smooth.
#[derive(Default)]
pub struct Cache {
    lang: Option<Lang>,
    text: Vec<u16>,
    spans: Vec<(usize, usize, Kind)>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Json,
    Toml,
    Xml,
    Log,
    Kotlin,
    Java,
}

impl Lang {
    pub fn from_path(path: &str) -> Option<Lang> {
        let name = path.rsplit(['\\', '/']).next()?;
        let (_, ext) = name.rsplit_once('.')?;
        match ext.to_ascii_lowercase().as_str() {
            "json" => Some(Lang::Json),
            "toml" => Some(Lang::Toml),
            "xml" => Some(Lang::Xml),
            "log" => Some(Lang::Log),
            "kt" | "kts" => Some(Lang::Kotlin),
            "java" => Some(Lang::Java),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Keyword,
    Type,
    String,
    Number,
    Comment,
    /// Keys, attribute names, annotations.
    Name,
    /// XML tags, TOML table headers.
    Tag,
    Error,
    Warning,
    Muted,
}

const KOTLIN_KEYWORDS: &[&str] = &[
    "abstract", "annotation", "as", "break", "by", "catch", "class", "companion", "const", "constructor", "continue",
    "crossinline", "data", "do", "else", "enum", "external", "false", "final", "finally", "for", "fun", "if", "import",
    "in", "infix", "init", "inline", "inner", "interface", "internal", "is", "lateinit", "noinline", "null", "object",
    "open", "operator", "out", "override", "package", "private", "protected", "public", "reified", "return", "sealed",
    "super", "suspend", "tailrec", "this", "throw", "true", "try", "typealias", "val", "value", "var", "vararg", "when",
    "where", "while",
];

const JAVA_KEYWORDS: &[&str] = &[
    "abstract", "assert", "boolean", "break", "byte", "case", "catch", "char", "class", "const", "continue", "default",
    "do", "double", "else", "enum", "extends", "false", "final", "finally", "float", "for", "goto", "if", "implements",
    "import", "instanceof", "int", "interface", "long", "native", "new", "null", "package", "permits", "private",
    "protected", "public", "record", "return", "sealed", "short", "static", "strictfp", "super", "switch",
    "synchronized", "this", "throw", "throws", "transient", "true", "try", "var", "void", "volatile", "while", "yield",
];

/// Colored runs (start, end, kind) that touch `range`, in order.
pub fn spans(lang: Lang, text: &[u16], range: (usize, usize)) -> Vec<(usize, usize, Kind)> {
    let mut out = Vec::new();
    let end = range.1.min(text.len());
    match lang {
        Lang::Json => json(text, end, &mut out),
        Lang::Toml => toml(text, end, &mut out),
        Lang::Xml => xml(text, end, &mut out),
        Lang::Log => log(text, end, &mut out),
        Lang::Kotlin => c_like(text, end, KOTLIN_KEYWORDS, true, &mut out),
        Lang::Java => c_like(text, end, JAVA_KEYWORDS, false, &mut out),
    }
    out.retain(|&(s, e, _)| e > range.0 && s < range.1);
    out
}

// ---------------------------------------------------------------- scanning

fn ch(c: u16) -> char {
    char::from_u32(c as u32).unwrap_or('\u{FFFD}')
}

pub fn is_ident(c: u16) -> bool {
    c == b'_' as u16 || ch(c).is_alphanumeric()
}

fn is_digit(c: u16) -> bool {
    (b'0' as u16..=b'9' as u16).contains(&c)
}

fn is_eol(c: u16) -> bool {
    c == b'\n' as u16 || c == b'\r' as u16
}

fn one_of(c: u16, set: &[u8]) -> bool {
    set.iter().any(|&x| c == x as u16)
}

fn at(t: &[u16], i: usize, s: &str) -> bool {
    (i..).zip(s.encode_utf16()).all(|(j, c)| t.get(j) == Some(&c))
}

fn word_is(w: &[u16], s: &str) -> bool {
    w.iter().copied().eq(s.encode_utf16())
}

/// Index just past `pat` at or after `from`, or the end of the text.
fn past(t: &[u16], from: usize, pat: &str) -> usize {
    let n = pat.encode_utf16().count();
    (from..t.len()).find(|&j| at(t, j, pat)).map_or(t.len(), |j| j + n)
}

fn line_end(t: &[u16], from: usize) -> usize {
    (from..t.len()).find(|&j| is_eol(t[j])).unwrap_or(t.len())
}

fn ident_end(t: &[u16], from: usize) -> usize {
    (from..t.len()).find(|&j| !is_ident(t[j])).unwrap_or(t.len())
}

fn skip_blanks(t: &[u16], from: usize) -> usize {
    (from..t.len()).find(|&j| t[j] != b' ' as u16 && t[j] != b'\t' as u16).unwrap_or(t.len())
}

fn at_line_start(t: &[u16], i: usize) -> bool {
    t[..i].iter().rev().find(|&&c| c != b' ' as u16 && c != b'\t' as u16).is_none_or(|&c| is_eol(c))
}

/// End of a string opened by `t[i]`. An unclosed one stops at the line end.
fn quoted(t: &[u16], i: usize, escapes: bool) -> usize {
    let q = t[i];
    let mut j = i + 1;
    while j < t.len() {
        let c = t[j];
        if escapes && c == b'\\' as u16 {
            j += 2;
            continue;
        }
        if c == q {
            return j + 1;
        }
        if is_eol(c) {
            return j;
        }
        j += 1;
    }
    t.len()
}

/// 0xFF, 1_000L, 1.5f, 1e10 (a dot only when a digit follows).
fn number_end(t: &[u16], from: usize) -> usize {
    let mut j = from;
    while j < t.len() && (is_ident(t[j]) || t[j] == b'.' as u16 && t.get(j + 1).is_some_and(|&c| is_digit(c))) {
        j += 1;
    }
    j
}

fn followed_by(t: &[u16], from: usize, c: u8) -> bool {
    t.get(skip_blanks(t, from)) == Some(&(c as u16))
}

// ---------------------------------------------------------------- languages

fn c_like(t: &[u16], end: usize, keywords: &[&str], raw_strings: bool, out: &mut Vec<(usize, usize, Kind)>) {
    let mut i = 0;
    while i < end {
        let c = t[i];
        let (j, kind) = if at(t, i, "//") {
            (line_end(t, i), Some(Kind::Comment))
        } else if at(t, i, "/*") {
            (past(t, i + 2, "*/"), Some(Kind::Comment))
        } else if raw_strings && at(t, i, "\"\"\"") {
            (past(t, i + 3, "\"\"\""), Some(Kind::String))
        } else if c == b'"' as u16 || c == b'\'' as u16 {
            (quoted(t, i, true), Some(Kind::String))
        } else if c == b'@' as u16 && t.get(i + 1).is_some_and(|&n| is_ident(n) && !is_digit(n)) {
            (ident_end(t, i + 1), Some(Kind::Name))
        } else if is_digit(c) {
            (number_end(t, i), Some(Kind::Number))
        } else if is_ident(c) {
            let j = ident_end(t, i);
            let w = &t[i..j];
            let kind = if keywords.iter().any(|k| word_is(w, k)) {
                Some(Kind::Keyword)
            } else if ch(c).is_uppercase() {
                Some(Kind::Type)
            } else {
                None
            };
            (j, kind)
        } else {
            (i + 1, None)
        };
        if let Some(k) = kind {
            out.push((i, j, k));
        }
        i = j;
    }
}

fn json(t: &[u16], end: usize, out: &mut Vec<(usize, usize, Kind)>) {
    let mut i = 0;
    while i < end {
        let c = t[i];
        let (j, kind) = if c == b'"' as u16 {
            let j = quoted(t, i, true);
            (j, Some(if followed_by(t, j, b':') { Kind::Name } else { Kind::String }))
        } else if is_digit(c) || c == b'-' as u16 && t.get(i + 1).is_some_and(|&n| is_digit(n)) {
            let mut j = i + 1;
            while j < t.len() && (is_digit(t[j]) || one_of(t[j], b".eE+-")) {
                j += 1;
            }
            (j, Some(Kind::Number))
        } else if is_ident(c) {
            let j = ident_end(t, i);
            let w = &t[i..j];
            let lit = ["true", "false", "null"].iter().any(|k| word_is(w, k));
            (j, lit.then_some(Kind::Keyword))
        } else if at(t, i, "//") {
            (line_end(t, i), Some(Kind::Comment))
        } else if at(t, i, "/*") {
            (past(t, i + 2, "*/"), Some(Kind::Comment))
        } else {
            (i + 1, None)
        };
        if let Some(k) = kind {
            out.push((i, j, k));
        }
        i = j;
    }
}

fn toml(t: &[u16], end: usize, out: &mut Vec<(usize, usize, Kind)>) {
    let bare = |c: u16| is_ident(c) || one_of(c, b"-.:+");
    let mut i = 0;
    while i < end {
        let c = t[i];
        let (j, kind) = if c == b'#' as u16 {
            (line_end(t, i), Some(Kind::Comment))
        } else if at(t, i, "\"\"\"") {
            (past(t, i + 3, "\"\"\""), Some(Kind::String))
        } else if at(t, i, "'''") {
            (past(t, i + 3, "'''"), Some(Kind::String))
        } else if c == b'"' as u16 || c == b'\'' as u16 {
            let j = quoted(t, i, c == b'"' as u16);
            (j, Some(if followed_by(t, j, b'=') { Kind::Name } else { Kind::String }))
        } else if c == b'[' as u16 && at_line_start(t, i) {
            let e = line_end(t, i);
            let mut j = (i..e).find(|&j| t[j] == b']' as u16).unwrap_or(e);
            while j < e && t[j] == b']' as u16 {
                j += 1;
            }
            (j, Some(Kind::Tag))
        } else if bare(c) {
            let j = (i..t.len()).find(|&j| !bare(t[j])).unwrap_or(t.len());
            let w = &t[i..j];
            let kind = if followed_by(t, j, b'=') {
                Some(Kind::Name)
            } else if is_digit(c) || (c == b'+' as u16 || c == b'-' as u16) && w.len() > 1 {
                Some(Kind::Number)
            } else if ["true", "false", "inf", "nan"].iter().any(|k| word_is(w, k)) {
                Some(Kind::Keyword)
            } else {
                None
            };
            (j, kind)
        } else {
            (i + 1, None)
        };
        if let Some(k) = kind {
            out.push((i, j, k));
        }
        i = j;
    }
}

fn xml(t: &[u16], end: usize, out: &mut Vec<(usize, usize, Kind)>) {
    let name = |c: u16| is_ident(c) || one_of(c, b":-.");
    let name_end = |from: usize| (from..t.len()).find(|&j| !name(t[j])).unwrap_or(t.len());
    let mut i = 0;
    while i < end {
        let c = t[i];
        if at(t, i, "<!--") {
            let j = past(t, i + 4, "-->");
            out.push((i, j, Kind::Comment));
            i = j;
        } else if at(t, i, "<![CDATA[") {
            let j = past(t, i + 9, "]]>");
            out.push((i, j, Kind::String));
            i = j;
        } else if at(t, i, "<?") {
            let j = past(t, i + 2, "?>");
            out.push((i, j, Kind::Muted));
            i = j;
        } else if at(t, i, "<!") {
            let j = past(t, i + 2, ">");
            out.push((i, j, Kind::Muted));
            i = j;
        } else if c == b'<' as u16 && t.get(i + 1).is_some_and(|&n| name(n) || n == b'/' as u16) {
            let open = if t[i + 1] == b'/' as u16 { i + 2 } else { i + 1 };
            let mut j = name_end(open);
            out.push((i, j, Kind::Tag));
            while j < t.len() {
                let c = t[j];
                if c == b'>' as u16 || at(t, j, "/>") {
                    let e = if c == b'>' as u16 { j + 1 } else { j + 2 };
                    out.push((j, e, Kind::Tag));
                    j = e;
                    break;
                } else if c == b'"' as u16 || c == b'\'' as u16 {
                    let e = past(t, j + 1, if c == b'"' as u16 { "\"" } else { "'" });
                    out.push((j, e, Kind::String));
                    j = e;
                } else if name(c) {
                    let e = name_end(j);
                    out.push((j, e, Kind::Name));
                    j = e;
                } else if c == b'<' as u16 {
                    break; // an unclosed tag: let the next one start fresh
                } else {
                    j += 1;
                }
            }
            i = j;
        } else if c == b'&' as u16 {
            let j = ident_end(t, i + 1);
            if j > i + 1 && t.get(j) == Some(&(b';' as u16)) {
                out.push((i, j + 1, Kind::Keyword));
                i = j + 1;
            } else {
                i += 1;
            }
        } else {
            i += 1;
        }
    }
}

/// Colored the way mclo.gs does it: a line that opens with a timestamp, a
/// bracket or a level starts an entry, and the lines under it (a stack trace)
/// belong to it. Warnings and errors color their whole entry, other levels
/// only the prefix.
fn log(t: &[u16], end: usize, out: &mut Vec<(usize, usize, Kind)>) {
    let mut level = None;
    let mut i = 0;
    while i < end {
        let e = line_end(t, i);
        if let Some((kind, prefix)) = log_entry(t, i, e) {
            level = kind;
            match kind {
                Some(k @ (Kind::Error | Kind::Warning)) => out.push((i, e, k)),
                Some(k) => out.push((i, prefix, k)),
                None => {}
            }
        } else if let Some(k @ (Kind::Error | Kind::Warning)) = level {
            out.push((i, e, k));
        } else if at(t, skip_blanks(t, i), "//") {
            out.push((i, e, Kind::Muted));
        } else if stack_line(t, i, e) {
            out.push((i, e, Kind::Error));
        }
        i = e;
        while i < t.len() && is_eol(t[i]) {
            i += 1;
        }
    }
}

/// For a line that starts an entry, its level and where its prefix ends
/// (past the level and a closing "]:").
fn log_entry(t: &[u16], from: usize, e: usize) -> Option<(Option<Kind>, usize)> {
    if from >= e {
        return None;
    }
    let starts = t[from] == b'[' as u16 || is_digit(t[from]);
    if !starts && !is_ident(t[from]) {
        return None;
    }
    let mut i = from;
    while i < e {
        if !is_ident(t[i]) {
            i += 1;
            continue;
        }
        let j = ident_end(t, i).min(e);
        if let Some(k) = log_level(&t[i..j]) {
            let p = (j..e).find(|&p| !one_of(t[p], b"]:")).unwrap_or(e);
            return Some((Some(k), p));
        }
        // Without a timestamp or bracket, only a leading level starts an entry.
        if !starts {
            return None;
        }
        i = j;
    }
    Some((None, from))
}

fn log_level(w: &[u16]) -> Option<Kind> {
    let is = |list: &[&str]| list.iter().any(|k| word_is(w, k));
    if is(&["ERROR", "FATAL", "SEVERE", "CRITICAL", "EMERGENCY"]) {
        Some(Kind::Error)
    } else if is(&["WARN", "WARNING"]) {
        Some(Kind::Warning)
    } else if is(&["INFO", "NOTICE", "DEBUG", "TRACE", "VERBOSE", "FINE"]) {
        Some(Kind::Keyword)
    } else {
        None
    }
}

/// A Java stack trace line: a frame, "Caused by:", "... 3 more" or the exception itself.
fn stack_line(t: &[u16], from: usize, e: usize) -> bool {
    let first = skip_blanks(t, from);
    if (first > from && at(t, first, "at ")) || ["Caused by:", "Suppressed:", "... "].iter().any(|s| at(t, first, s)) {
        return true;
    }
    let j = (first..e).find(|&j| !is_ident(t[j]) && !one_of(t[j], b".$")).unwrap_or(e);
    let w = &t[first..j];
    (j == e || t[j] == b':' as u16) && ["Exception", "Error", "Throwable"].iter().any(|s| ends_with(w, s))
}

fn ends_with(w: &[u16], s: &str) -> bool {
    let s: Vec<u16> = s.encode_utf16().collect();
    w.ends_with(&s)
}

// ---------------------------------------------------------------- painting

unsafe fn pos_from_char(edit: HWND, i: usize) -> Option<(i32, i32)> {
    let r = SendMessageW(edit, EM_POSFROMCHAR, i, 0);
    (r != -1).then_some(((r & 0xFFFF) as i16 as i32, ((r >> 16) & 0xFFFF) as i16 as i32))
}

/// The control's text and, when `lang` is set, its runs, lexed again only when it changed.
unsafe fn refresh(edit: HWND, lang: Option<Lang>, cache: &mut Cache) {
    let len = GetWindowTextLengthW(edit);
    let mut text = vec![0u16; len as usize + 1];
    let got = GetWindowTextW(edit, text.as_mut_ptr(), len + 1);
    text.truncate(got.max(0) as usize);
    if cache.lang != lang || cache.text != text {
        cache.spans = lang.map_or_else(Vec::new, |lang| spans(lang, &text, (0, text.len())));
        cache.lang = lang;
        cache.text = text;
    }
}

/// The visible lines drawn whole at zoom `to` while the control still has
/// them laid out at `from`, with `font` at `to`: a crisp stand-in until it
/// re-lays them out. Tabs use the control's default stops (8 average
/// characters); a wrapped line keeps its old break until then.
pub unsafe fn paint_zoomed(edit: HWND, hdc: HDC, bottom: i32, lang: Option<Lang>, font: HFONT, (from, to): (i32, i32), cache: &mut Cache) {
    refresh(edit, lang.filter(|_| GetWindowTextLengthW(edit) <= MAX_CHARS), cache);
    let (text, spans) = (&cache.text, &cache.spans);
    let mut rc: RECT = std::mem::zeroed();
    SendMessageW(edit, EM_GETRECT, 0, &mut rc as *mut RECT as isize);
    let first = SendMessageW(edit, EM_GETFIRSTVISIBLELINE, 0, 0) as usize;
    // Scrolled sideways: the hidden width scales with the text.
    let start = SendMessageW(edit, EM_LINEINDEX, first, 0).max(0) as usize;
    let x = pos_from_char(edit, start).map_or(rc.left, |(x, _)| rc.left - muldiv(rc.left - x, to, from));

    let saved = SaveDC(hdc);
    IntersectClipRect(hdc, rc.left, rc.top, rc.right, bottom);
    SelectObject(hdc, font);
    SetBkMode(hdc, TRANSPARENT as i32);
    let mut tm: TEXTMETRICW = std::mem::zeroed();
    GetTextMetricsW(hdc, &mut tm);
    let colors = theme::syntax();
    let draw = |ls: usize, a: usize, b: usize, y: i32, color| {
        if a >= b {
            return;
        }
        let dx = if a > ls { (GetTabbedTextExtentW(hdc, text[ls..].as_ptr(), (a - ls) as i32, 0, std::ptr::null()) & 0xFFFF) as i32 } else { 0 };
        SetTextColor(hdc, color);
        TabbedTextOutW(hdc, x + dx, y, text[a..].as_ptr(), (b - a) as i32, 0, std::ptr::null(), x);
    };
    let mut y = rc.top;
    for line in first.. {
        if y >= bottom {
            break;
        }
        let ls = SendMessageW(edit, EM_LINEINDEX, line, 0);
        if ls < 0 {
            break;
        }
        let ls = ls as usize;
        let le = (ls + SendMessageW(edit, EM_LINELENGTH, ls, 0) as usize).min(text.len());
        // Plain stretches and runs side by side: drawing one over the other
        // would blend their antialiased edges.
        let mut p = ls;
        for &(s, e, kind) in spans[spans.partition_point(|sp| sp.1 <= ls)..].iter().take_while(|sp| sp.0 < le) {
            let (s, e) = (s.max(ls), e.min(le));
            draw(ls, p, s, y, theme::edit_fg());
            draw(ls, s, e, y, colors.color(kind));
            p = e;
        }
        draw(ls, p, le, y, theme::edit_fg());
        y += tm.tmHeight;
    }
    RestoreDC(hdc, saved);
}

/// Draw the colored runs of the visible lines over what the control painted.
/// `font` must be the font the control draws with at its current zoom.
pub unsafe fn paint(edit: HWND, hdc: HDC, lang: Lang, font: HFONT, cache: &mut Cache) {
    let len = GetWindowTextLengthW(edit);
    if len == 0 || len > MAX_CHARS {
        return;
    }
    refresh(edit, Some(lang), cache);
    let text = &cache.text;

    let mut rc: RECT = std::mem::zeroed();
    SendMessageW(edit, EM_GETRECT, 0, &mut rc as *mut RECT as isize);
    let first = SendMessageW(edit, EM_GETFIRSTVISIBLELINE, 0, 0) as usize;
    let count = SendMessageW(edit, EM_GETLINECOUNT, 0, 0) as usize;
    let mut lines = Vec::new();
    for line in first..count {
        let start = SendMessageW(edit, EM_LINEINDEX, line, 0);
        if start < 0 {
            break;
        }
        let start = start as usize;
        let pos = pos_from_char(edit, start);
        if pos.is_some_and(|(_, y)| y >= rc.bottom) {
            break;
        }
        let n = SendMessageW(edit, EM_LINELENGTH, start, 0) as usize;
        lines.push((start, (start + n).min(text.len()), pos));
    }
    let (Some(&(top, _, _)), Some(&(_, bottom, _))) = (lines.first(), lines.last()) else { return };
    // Runs are in order and don't overlap, so their ends are sorted too.
    let spans = &cache.spans[cache.spans.partition_point(|sp| sp.1 <= top)..cache.spans.partition_point(|sp| sp.0 < bottom)];
    if spans.is_empty() {
        return;
    }
    let (mut sel_start, mut sel_end) = (0u32, 0u32);
    SendMessageW(edit, EM_GETSEL, &mut sel_start as *mut u32 as usize, &mut sel_end as *mut u32 as isize);
    let sel = (sel_start as usize, sel_end as usize);

    HideCaret(edit);
    let saved = SaveDC(hdc);
    IntersectClipRect(hdc, rc.left, rc.top, rc.right, rc.bottom);
    SelectObject(hdc, font);
    SetBkMode(hdc, OPAQUE as i32);
    SetBkColor(hdc, theme::edit_bg());
    let mut tm: TEXTMETRICW = std::mem::zeroed();
    GetTextMetricsW(hdc, &mut tm);
    let colors = theme::syntax();
    for &(ls, le, pos) in &lines {
        let mut anchor = pos.map(|(x, y)| (ls, x, y));
        for &(s, e, kind) in spans.iter().filter(|sp| sp.1 > ls && sp.0 < le) {
            let (s, e) = (s.max(ls), e.min(le));
            // Selected text keeps the control's highlight.
            let pieces = [(s, e.min(sel.0)), (s.max(sel.1), e)];
            let pieces: &[(usize, usize)] = if sel.0 < sel.1 { &pieces } else { &[(s, e)] };
            SetTextColor(hdc, colors.color(kind));
            for &(a, b) in pieces {
                draw_run(edit, hdc, text, a, b, tm.tmHeight, &mut anchor);
            }
        }
    }
    RestoreDC(hdc, saved);
    ShowCaret(edit);
}

/// One run, split at tabs so the tab stops stay the control's. Asking the
/// control where a character is re-measures its line (most of a repaint), so
/// it is measured on from `anchor`, a known (char, x, y) on the line, up to
/// the next tab.
unsafe fn draw_run(edit: HWND, hdc: HDC, text: &[u16], a: usize, b: usize, height: i32, anchor: &mut Option<(usize, i32, i32)>) {
    let mut p = a;
    while p < b {
        if text[p] == b'\t' as u16 {
            p += 1;
            continue;
        }
        let q = (p..b).find(|&j| text[j] == b'\t' as u16).unwrap_or(b);
        let pos = match *anchor {
            Some((i, x, y)) if i <= p && !text[i..p].contains(&(b'\t' as u16)) => {
                let mut gap: SIZE = std::mem::zeroed();
                GetTextExtentPoint32W(hdc, text[i..].as_ptr(), (p - i) as i32, &mut gap);
                Some((x + gap.cx, y))
            }
            _ => pos_from_char(edit, p),
        };
        if let Some((x, y)) = pos {
            let run = &text[p..q];
            let mut size: SIZE = std::mem::zeroed();
            GetTextExtentPoint32W(hdc, run.as_ptr(), run.len() as i32, &mut size);
            let r = RECT { left: x, top: y, right: x + size.cx, bottom: y + height };
            ExtTextOutW(hdc, x, y, ETO_OPAQUE | ETO_CLIPPED, &r, run.as_ptr(), run.len() as u32, std::ptr::null());
            *anchor = Some((q, x + size.cx, y));
        }
        p = q;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn lex(lang: Lang, s: &str) -> Vec<(String, Kind)> {
        let t = w(s);
        spans(lang, &t, (0, t.len())).into_iter().map(|(a, b, k)| (String::from_utf16_lossy(&t[a..b]), k)).collect()
    }

    fn has(v: &[(String, Kind)], s: &str, k: Kind) -> bool {
        v.iter().any(|(t, kind)| t == s && *kind == k)
    }

    #[test]
    fn languages_by_extension() {
        assert_eq!(Lang::from_path(r"C:\a\b.JSON"), Some(Lang::Json));
        assert_eq!(Lang::from_path("build.gradle.kts"), Some(Lang::Kotlin));
        assert_eq!(Lang::from_path(r"C:\x.y\notes"), None);
        assert_eq!(Lang::from_path("notes.txt"), None);
    }

    #[test]
    fn json_keys_and_values() {
        let v = lex(Lang::Json, r#"{"name": "x", "n": -1.5e3, "ok": true}"#);
        assert!(has(&v, "\"name\"", Kind::Name));
        assert!(has(&v, "\"x\"", Kind::String));
        assert!(has(&v, "-1.5e3", Kind::Number));
        assert!(has(&v, "true", Kind::Keyword));
    }

    #[test]
    fn kotlin_block_comment_spans_lines() {
        let t = w("val a = 1\n/* one\ntwo */ fun f() = \"s\"");
        let start = t.iter().position(|&c| c == b't' as u16).unwrap();
        let v = spans(Lang::Kotlin, &t, (start, t.len()));
        assert_eq!(v[0].2, Kind::Comment);
        assert!(v[0].0 < start);
        let v = lex(Lang::Kotlin, "@Serializable data class P(val s: String = \"\"\"raw\n\"\"\")");
        assert!(has(&v, "@Serializable", Kind::Name));
        assert!(has(&v, "data", Kind::Keyword));
        assert!(has(&v, "String", Kind::Type));
        assert!(has(&v, "\"\"\"raw\n\"\"\"", Kind::String));
    }

    #[test]
    fn java_keywords_and_numbers() {
        let v = lex(Lang::Java, "@Override public int f() { return 0xFF + 1_000L; } // done");
        assert!(has(&v, "@Override", Kind::Name));
        assert!(has(&v, "int", Kind::Keyword));
        assert!(has(&v, "0xFF", Kind::Number));
        assert!(has(&v, "1_000L", Kind::Number));
        assert!(has(&v, "// done", Kind::Comment));
        assert!(!v.iter().any(|(t, _)| t == "f"));
    }

    #[test]
    fn xml_tags_and_attributes() {
        let v = lex(Lang::Xml, "<?xml version=\"1.0\"?>\n<!-- c -->\n<a:b x=\"1\" y='2'>&amp;</a:b>");
        assert!(has(&v, "<?xml version=\"1.0\"?>", Kind::Muted));
        assert!(has(&v, "<!-- c -->", Kind::Comment));
        assert!(has(&v, "<a:b", Kind::Tag));
        assert!(has(&v, "x", Kind::Name));
        assert!(has(&v, "'2'", Kind::String));
        assert!(has(&v, "&amp;", Kind::Keyword));
        assert!(has(&v, "</a:b", Kind::Tag));
    }

    #[test]
    fn toml_tables_and_keys() {
        let v = lex(Lang::Toml, "# c\n[package]\nname = \"x\"\n[[bin]]\ndeps.a = { v = 1, on = true }\nat = 1979-05-27T07:32:00Z");
        assert!(has(&v, "# c", Kind::Comment));
        assert!(has(&v, "[package]", Kind::Tag));
        assert!(has(&v, "[[bin]]", Kind::Tag));
        assert!(has(&v, "name", Kind::Name));
        assert!(has(&v, "deps.a", Kind::Name));
        assert!(has(&v, "v", Kind::Name));
        assert!(has(&v, "true", Kind::Keyword));
        assert!(has(&v, "1979-05-27T07:32:00Z", Kind::Number));
    }

    #[test]
    fn log_levels() {
        let v = lex(Lang::Log, "[12:00:01] [Server thread/INFO]: Done\n2026-11-06 12:00:01,123 ERROR boom\n\tat a.B.c(B.java:1)\n[main] WARN slow\nmore");
        assert!(has(&v, "[12:00:01] [Server thread/INFO]:", Kind::Keyword));
        assert!(has(&v, "2026-11-06 12:00:01,123 ERROR boom", Kind::Error));
        assert!(has(&v, "\tat a.B.c(B.java:1)", Kind::Error));
        assert!(has(&v, "[main] WARN slow", Kind::Warning));
        assert!(has(&v, "more", Kind::Warning));
        let v = lex(Lang::Log, "[12:00] [main/INFO]: Crash\n// Oops\njava.lang.IllegalStateException: no\n\tat a.B.c(B.java:1)\nplain");
        assert!(has(&v, "// Oops", Kind::Muted));
        assert!(has(&v, "java.lang.IllegalStateException: no", Kind::Error));
        assert!(has(&v, "\tat a.B.c(B.java:1)", Kind::Error));
        assert!(!v.iter().any(|(t, _)| t == "plain"));
    }
}
