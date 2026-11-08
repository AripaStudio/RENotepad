//! Text rules that don't touch windows: .LOG, find, replace-all, line commands, command line.

use windows_sys::Win32::Globalization::{
    FindNLSString, GetDateFormatW, GetTimeFormatW, FIND_FROMEND, FIND_FROMSTART, NORM_IGNORECASE,
};

const LOCALE_USER_DEFAULT: u32 = 0x400;
const DATE_SHORTDATE: u32 = 1;
const TIME_NOSECONDS: u32 = 2;

/// Documents whose first four characters are exactly ".LOG" (case-sensitive)
/// get a timestamp appended every time they are opened.
pub fn is_log_file(text: &[u16]) -> bool {
    text.starts_with(&[b'.' as u16, b'L' as u16, b'O' as u16, b'G' as u16])
}

/// "h:mm tt M/d/yyyy" in the user's locale: time without seconds, short date.
/// F5 inserts it bare; the .LOG feature wraps it in CRLFs.
pub fn timestamp(for_log: bool) -> Vec<u16> {
    let mut time = [0u16; 80];
    let mut date = [0u16; 80];
    unsafe {
        let t = GetTimeFormatW(LOCALE_USER_DEFAULT, TIME_NOSECONDS, std::ptr::null(), std::ptr::null(), time.as_mut_ptr(), 80);
        let d = GetDateFormatW(LOCALE_USER_DEFAULT, DATE_SHORTDATE, std::ptr::null(), std::ptr::null(), date.as_mut_ptr(), 80);
        let crlf = [13u16, 10];
        let mut out = Vec::new();
        if for_log {
            out.extend_from_slice(&crlf);
        }
        out.extend_from_slice(&time[..(t.max(1) - 1) as usize]);
        out.push(b' ' as u16);
        out.extend_from_slice(&date[..(d.max(1) - 1) as usize]);
        if for_log {
            out.extend_from_slice(&crlf);
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub start: usize,
    pub len: usize,
    /// The search ran off one end and continued from the other.
    pub wrapped: bool,
}

/// Locale-aware search (FindNLSString, as Notepad uses), from `sel` = current
/// selection. Down searches after the selection start + 1 so repeated Find Next
/// moves forward; up searches strictly before the selection start.
pub fn find(text: &[u16], pat: &[u16], sel: (usize, usize), down: bool, match_case: bool, wrap: bool) -> Option<Hit> {
    if pat.is_empty() || text.is_empty() {
        return None;
    }
    let (sel_start, sel_end) = sel;
    if down {
        // Win10: when there's a selection, search starts at its end; else at the caret.
        let from = sel_end.min(text.len());
        if let Some((s, l)) = nls_find(&text[from..], pat, false, match_case) {
            return Some(Hit { start: from + s, len: l, wrapped: false });
        }
        if wrap {
            // Search the head up to the end of the selection so a match straddling it is found.
            let end = (sel_end + pat.len()).min(text.len());
            if let Some((s, l)) = nls_find(&text[..end], pat, false, match_case) {
                return Some(Hit { start: s, len: l, wrapped: true });
            }
        }
    } else {
        let to = sel_start.min(text.len());
        if let Some((s, l)) = nls_find(&text[..to], pat, true, match_case) {
            return Some(Hit { start: s, len: l, wrapped: false });
        }
        if wrap {
            let from = sel_start.saturating_sub(pat.len());
            if let Some((s, l)) = nls_find(&text[from..], pat, true, match_case) {
                return Some(Hit { start: from + s, len: l, wrapped: true });
            }
        }
    }
    None
}

fn nls_find(hay: &[u16], pat: &[u16], from_end: bool, match_case: bool) -> Option<(usize, usize)> {
    if hay.is_empty() {
        return None;
    }
    let mut flags = if from_end { FIND_FROMEND } else { FIND_FROMSTART };
    if !match_case {
        flags |= NORM_IGNORECASE;
    }
    let mut found_len: i32 = 0;
    let idx = unsafe {
        FindNLSString(LOCALE_USER_DEFAULT, flags, hay.as_ptr(), hay.len() as i32, pat.as_ptr(), pat.len() as i32, &mut found_len)
    };
    (idx >= 0 && found_len > 0).then_some((idx as usize, found_len as usize))
}

/// Every non-overlapping match replaced, start to end. Returns (new text, count).
pub fn replace_all(text: &[u16], pat: &[u16], with: &[u16], match_case: bool) -> (Vec<u16>, usize) {
    let mut out = Vec::with_capacity(text.len());
    let mut pos = 0;
    let mut count = 0;
    while pos < text.len() {
        match nls_find(&text[pos..], pat, false, match_case) {
            Some((s, l)) => {
                out.extend_from_slice(&text[pos..pos + s]);
                out.extend_from_slice(with);
                pos += s + l;
                count += 1;
            }
            None => break,
        }
    }
    out.extend_from_slice(&text[pos.min(text.len())..]);
    (out, count)
}

/// Does the selected text equal the search string (for Replace)?
pub fn selection_matches(sel: &[u16], pat: &[u16], match_case: bool) -> bool {
    !sel.is_empty() && nls_find(sel, pat, false, match_case) == Some((0, sel.len()))
}

const CR: u16 = 0x0D;
const LF: u16 = 0x0A;
const TAB: u16 = 0x09;
const SPACE: u16 = 0x20;

/// What a line command does to the control: `start..end` becomes `text`,
/// then `sel` is selected (anchor, caret).
#[derive(Debug, PartialEq, Eq)]
pub struct LineEdit {
    pub start: usize,
    pub end: usize,
    pub text: Vec<u16>,
    pub sel: (usize, usize),
}

fn is_break(c: u16) -> bool {
    c == CR || c == LF
}

pub fn line_start(text: &[u16], mut p: usize) -> usize {
    while p > 0 && !is_break(text[p - 1]) {
        p -= 1;
    }
    p
}

fn line_end(text: &[u16], mut p: usize) -> usize {
    while p < text.len() && !is_break(text[p]) {
        p += 1;
    }
    p
}

/// The line break at `p` (CRLF, CR or LF; mixed files keep all three): its length, 0 at the end.
fn break_len(text: &[u16], p: usize) -> usize {
    match text.get(p) {
        Some(&CR) if text.get(p + 1) == Some(&LF) => 2,
        Some(&c) if is_break(c) => 1,
        _ => 0,
    }
}

/// The line break ending just before `p`: its length.
fn break_before(text: &[u16], p: usize) -> usize {
    if p >= 2 && text[p - 2] == CR && text[p - 1] == LF {
        return 2;
    }
    (p >= 1 && is_break(text[p - 1])) as usize
}

/// The spaces and tabs at the start of the line that starts at `ls`.
fn indent_at(text: &[u16], ls: usize) -> &[u16] {
    let mut p = ls;
    while p < text.len() && matches!(text[p], SPACE | TAB) {
        p += 1;
    }
    &text[ls..p]
}

/// The whole lines the selection touches: (start, end of their text, end after
/// the last one's break). A selection that ends at the start of a line leaves
/// that line out, as VS Code does.
fn block(text: &[u16], (s, e): (usize, usize)) -> (usize, usize, usize) {
    let last = if e > s && line_start(text, e) == e { e - break_before(text, e) } else { e };
    let end = line_end(text, last);
    (line_start(text, s), end, end + break_len(text, end))
}

/// Alt+Up / Alt+Down: the lines swap with the one above or below, each line
/// break staying where it was. None at the top or bottom.
pub fn move_lines(text: &[u16], sel: (usize, usize), up: bool) -> Option<LineEdit> {
    let (bs, bc, be) = block(text, sel);
    if up {
        if bs == 0 {
            return None;
        }
        let above_end = bs - break_before(text, bs);
        let above = line_start(text, above_end);
        let mut out = text[bs..bc].to_vec();
        out.extend_from_slice(&text[above_end..bs]);
        out.extend_from_slice(&text[above..above_end]);
        out.extend_from_slice(&text[bc..be]);
        // A selection ending at the next line's start keeps ending after the moved lines.
        let after = above + (bc - bs) + (bs - above_end);
        let map = |p: usize| if p <= bc { p - (bs - above) } else { after };
        return Some(LineEdit { start: above, end: be, text: out, sel: (map(sel.0), map(sel.1)) });
    }
    if be == bc {
        return None;
    }
    let below_end = line_end(text, be);
    let end = below_end + break_len(text, below_end);
    let mut out = text[be..below_end].to_vec();
    out.extend_from_slice(&text[bc..be]);
    out.extend_from_slice(&text[bs..bc]);
    out.extend_from_slice(&text[below_end..end]);
    let map = |p: usize| if p <= bc { p + (below_end - bc) } else { end };
    Some(LineEdit { start: bs, end, text: out, sel: (map(sel.0), map(sel.1)) })
}

/// Shift+Alt+Up / Shift+Alt+Down: the lines are copied above or below; the
/// selection follows the lower copy going down and stays on the upper one going up.
pub fn copy_lines(text: &[u16], sel: (usize, usize), up: bool, eol: &[u16]) -> LineEdit {
    let (bs, bc, be) = block(text, sel);
    let brk = if be > bc { &text[bc..be] } else { eol };
    if up {
        let out = [&text[bs..bc], brk].concat();
        return LineEdit { start: bs, end: bs, text: out, sel };
    }
    let out = [brk, &text[bs..bc]].concat();
    let d = out.len();
    LineEdit { start: bc, end: bc, text: out, sel: (sel.0 + d, sel.1 + d) }
}

/// Ctrl+Shift+K: the lines go, with their break (or, on the last line, the
/// break before them). The caret keeps its column on the line that takes their place.
pub fn delete_lines(text: &[u16], sel: (usize, usize)) -> LineEdit {
    let (bs, bc, be) = block(text, sel);
    let col = sel.0 - bs;
    if be > bc || bs == 0 {
        let caret = bs + col.min(line_end(text, be) - be);
        return LineEdit { start: bs, end: be, text: Vec::new(), sel: (caret, caret) };
    }
    let start = bs - break_before(text, bs);
    let above = line_start(text, start);
    let caret = above + col.min(start - above);
    LineEdit { start, end: be, text: Vec::new(), sel: (caret, caret) }
}

/// Ctrl+L: the whole line with its break; again, and the next line joins it.
pub fn select_line(text: &[u16], (s, e): (usize, usize)) -> (usize, usize) {
    let end = line_end(text, e);
    (line_start(text, s), end + break_len(text, end))
}

/// Ctrl+Enter / Ctrl+Shift+Enter: a new line below or above, indented like this one.
pub fn insert_line(text: &[u16], (s, e): (usize, usize), above: bool, eol: &[u16]) -> LineEdit {
    if above {
        let at = line_start(text, s);
        let indent = indent_at(text, at);
        let caret = at + indent.len();
        return LineEdit { start: at, end: at, text: [indent, eol].concat(), sel: (caret, caret) };
    }
    let at = line_end(text, e);
    let out = [eol, indent_at(text, line_start(text, e))].concat();
    let caret = at + out.len();
    LineEdit { start: at, end: at, text: out, sel: (caret, caret) }
}

/// Enter keeps the line's indentation, as far as the caret.
pub fn enter_indent(text: &[u16], caret: usize) -> &[u16] {
    let ls = line_start(text, caret);
    let indent = indent_at(text, ls);
    &indent[..indent.len().min(caret - ls)]
}

/// Home goes to the line's first non-blank character, or from there to column 0.
pub fn smart_home(text: &[u16], caret: usize) -> usize {
    let ls = line_start(text, caret);
    let first = ls + indent_at(text, ls).len();
    if caret == first { ls } else { first }
}

/// One level of indentation: a tab, unless the file indents with spaces (then four).
pub fn indent_unit(text: &[u16]) -> Vec<u16> {
    let mut p = 0;
    while p < text.len() {
        match text[p] {
            TAB => return vec![TAB],
            SPACE => return vec![SPACE; 4],
            _ => {}
        }
        let le = line_end(text, p);
        p = le + break_len(text, le).max(1);
    }
    vec![TAB]
}

/// Ctrl+] / Tab over lines adds `unit` to each non-empty line; Ctrl+[ /
/// Shift+Tab takes off a tab or up to four spaces. None when nothing changes.
pub fn indent_lines(text: &[u16], sel: (usize, usize), unit: &[u16], outdent: bool) -> Option<LineEdit> {
    let (bs, bc, _) = block(text, sel);
    let mut out = Vec::with_capacity(bc - bs + unit.len());
    // (line start, characters added (+) or removed (-) there)
    let mut changes: Vec<(usize, isize)> = Vec::new();
    let mut p = bs;
    loop {
        let le = line_end(text, p);
        let line = &text[p..le];
        let change = if outdent {
            let n = if line.first() == Some(&TAB) { 1 } else { line.iter().take(4).take_while(|&&c| c == SPACE).count() };
            out.extend_from_slice(&line[n..]);
            -(n as isize)
        } else {
            let add = if line.is_empty() { &[][..] } else { unit };
            out.extend_from_slice(add);
            out.extend_from_slice(line);
            add.len() as isize
        };
        changes.push((p, change));
        if le >= bc {
            break;
        }
        let bl = break_len(text, le);
        out.extend_from_slice(&text[le..le + bl]);
        p = le + bl;
    }
    if changes.iter().all(|&(_, c)| c == 0) {
        return None;
    }
    // A selection starting at a line start keeps starting there, so whole lines stay selected.
    let map = |pos: usize, starts_sel: bool| {
        let mut shift = 0isize;
        for &(ls, c) in &changes {
            if ls > pos || (ls == pos && starts_sel) {
                break;
            }
            // Removed blanks the position was inside of pull it back to the line start.
            shift += if c < 0 { -((pos - ls).min(-c as usize) as isize) } else { c };
        }
        (pos as isize + shift) as usize
    };
    let starts = sel.0 != sel.1;
    Some(LineEdit { start: bs, end: bc, text: out, sel: (map(sel.0, starts), map(sel.1, false)) })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Args {
    /// /A or /W: force ANSI / UTF-16 LE when opening.
    pub force_ansi: bool,
    pub force_unicode: bool,
    /// /P: print the file to the default printer and exit.
    pub print: bool,
    /// /ImportNotepadPins: copy Notepad's jump list pins, no window (RENotepad only).
    pub import_pins: bool,
    /// /PinToJumpList file: pin the file to the jump list, no window (RENotepad only).
    pub pin: bool,
    pub file: Option<String>,
}

/// The long switch at the start of `rest`, if it's `name` as a whole word:
/// what follows it.
fn long_switch<'a>(rest: &'a str, name: &str) -> Option<&'a str> {
    let (head, tail) = (rest.get(..name.len())?, rest.get(name.len()..)?);
    (head.eq_ignore_ascii_case(name) && tail.chars().next().is_none_or(|c| c == ' ' || c == '\t')).then_some(tail)
}

/// Notepad takes the rest of the command line as one filename, quoted or not
/// ("notepad my notes.txt" works), after optional /A, /W or /P switches.
pub fn parse_args(cmdline: &str) -> Args {
    let mut rest = skip_program_name(cmdline).trim_start();
    let mut args = Args::default();
    loop {
        if let Some(tail) = long_switch(rest, "/ImportNotepadPins") {
            args.import_pins = true;
            rest = tail.trim_start();
            continue;
        }
        if let Some(tail) = long_switch(rest, "/PinToJumpList") {
            args.pin = true;
            rest = tail.trim_start();
            continue;
        }
        let lower = rest.get(..2).map(str::to_ascii_lowercase);
        let switch_end = rest.as_bytes().get(2).is_none_or(|c| *c == b' ' || *c == b'\t');
        match lower.as_deref() {
            Some("/a") if switch_end => args.force_ansi = true,
            Some("/w") if switch_end => args.force_unicode = true,
            Some("/p") if switch_end => args.print = true,
            _ => break,
        }
        rest = rest[2..].trim_start();
    }
    let file = rest.trim();
    let file = file.strip_prefix('"').map_or(file, |f| f.strip_suffix('"').unwrap_or(f));
    if !file.is_empty() {
        args.file = Some(file.to_string());
    }
    args
}

fn skip_program_name(cmdline: &str) -> &str {
    let s = cmdline.trim_start();
    if let Some(r) = s.strip_prefix('"') {
        r.find('"').map_or("", |i| &r[i + 1..])
    } else {
        s.find([' ', '\t']).map_or("", |i| &s[i..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn log_needs_exact_prefix() {
        assert!(is_log_file(&w(".LOG\r\nfirst")));
        assert!(!is_log_file(&w(".log\r\n")));
        assert!(!is_log_file(&w(" .LOG")));
        assert!(!is_log_file(&w(".LO")));
    }

    #[test]
    fn timestamp_shapes() {
        let bare = String::from_utf16(&timestamp(false)).unwrap();
        assert!(!bare.contains('\n') && bare.contains(' '), "{bare:?}");
        let log = String::from_utf16(&timestamp(true)).unwrap();
        assert!(log.starts_with("\r\n") && log.ends_with("\r\n"));
    }

    #[test]
    fn find_down_and_up() {
        let t = w("one two one two");
        let p = w("two");
        assert_eq!(find(&t, &p, (0, 0), true, false, false).unwrap().start, 4);
        assert_eq!(find(&t, &p, (4, 7), true, false, false).unwrap().start, 12);
        assert_eq!(find(&t, &p, (12, 15), false, false, false).unwrap().start, 4);
    }

    #[test]
    fn find_wraps_only_when_asked() {
        let t = w("abc xyz abc");
        let p = w("xyz");
        assert_eq!(find(&t, &p, (8, 11), true, false, false), None);
        assert_eq!(find(&t, &p, (8, 11), true, false, true), Some(Hit { start: 4, len: 3, wrapped: true }));
        assert_eq!(find(&t, &p, (0, 0), false, false, true), Some(Hit { start: 4, len: 3, wrapped: true }));
    }

    #[test]
    fn find_case() {
        let t = w("Hello hello");
        assert_eq!(find(&t, &w("hello"), (0, 0), true, true, false).unwrap().start, 6);
        assert_eq!(find(&t, &w("hello"), (0, 0), true, false, false).unwrap().start, 0);
    }

    #[test]
    fn replace_all_counts() {
        let (out, n) = replace_all(&w("a-b-c"), &w("-"), &w("--"), true);
        assert_eq!((String::from_utf16(&out).unwrap(), n), ("a--b--c".into(), 2));
        let (out, n) = replace_all(&w("Cat cat"), &w("cat"), &w("dog"), false);
        assert_eq!((String::from_utf16(&out).unwrap(), n), ("dog dog".into(), 2));
    }

    /// The text after `e`, and what's selected in it.
    fn apply(t: &str, e: LineEdit) -> (String, String) {
        let mut t = w(t);
        t.splice(e.start..e.end, e.text);
        let (a, b) = (e.sel.0.min(e.sel.1), e.sel.0.max(e.sel.1));
        (String::from_utf16(&t).unwrap(), String::from_utf16(&t[a..b]).unwrap())
    }

    #[test]
    fn move_lines_keeps_each_break() {
        let t = "a\r\nbb\ncc";
        assert_eq!(apply(t, move_lines(&w(t), (4, 5), true).unwrap()), ("bb\r\na\ncc".into(), "b".into()));
        assert_eq!(apply(t, move_lines(&w(t), (0, 0), false).unwrap()), ("bb\r\na\ncc".into(), "".into()));
        assert_eq!(apply(t, move_lines(&w(t), (4, 4), false).unwrap()), ("a\r\ncc\nbb".into(), "".into()));
        assert_eq!(move_lines(&w(t), (1, 1), true), None);
        assert_eq!(move_lines(&w(t), (7, 8), false), None);
    }

    #[test]
    fn move_whole_lines_keeps_them_selected() {
        let t = "a\nb\nc\n";
        // "b\n" selected: the selection's end is the next line's start
        assert_eq!(apply(t, move_lines(&w(t), (2, 4), true).unwrap()), ("b\na\nc\n".into(), "b\n".into()));
        assert_eq!(apply(t, move_lines(&w(t), (2, 4), false).unwrap()), ("a\nc\nb\n".into(), "b\n".into()));
        // onto the empty last line
        assert_eq!(apply(t, move_lines(&w(t), (4, 4), false).unwrap()).0, "a\nb\n\nc");
    }

    #[test]
    fn copy_lines_up_and_down() {
        let t = "a\r\nbc";
        assert_eq!(apply(t, copy_lines(&w(t), (4, 4), false, &w("\r\n"))), ("a\r\nbc\r\nbc".into(), "".into()));
        let e = copy_lines(&w(t), (4, 4), false, &w("\r\n"));
        assert_eq!(e.sel, (8, 8));
        let e = copy_lines(&w(t), (0, 1), true, &w("\n"));
        assert_eq!((apply(t, e).0, copy_lines(&w(t), (0, 1), true, &w("\n")).sel), ("a\r\na\r\nbc".into(), (0, 1)));
    }

    #[test]
    fn delete_lines_keeps_the_column() {
        let t = "abc\nd\nefgh";
        assert_eq!(delete_lines(&w(t), (2, 2)), LineEdit { start: 0, end: 4, text: vec![], sel: (1, 1) });
        // the last line takes the break before it
        assert_eq!(apply("abc\nd\nefgh", delete_lines(&w(t), (9, 9))).0, "abc\nd");
        assert_eq!(delete_lines(&w(t), (9, 9)).sel, (5, 5));
        assert_eq!(apply("x", delete_lines(&w("x"), (0, 0))).0, "");
    }

    #[test]
    fn select_line_grows() {
        let t = w("ab\r\ncd\r\nef");
        assert_eq!(select_line(&t, (1, 1)), (0, 4));
        assert_eq!(select_line(&t, (0, 4)), (0, 8));
        assert_eq!(select_line(&t, (0, 8)), (0, 10));
    }

    #[test]
    fn insert_line_copies_the_indent() {
        let t = "  ab\ncd";
        assert_eq!(apply(t, insert_line(&w(t), (3, 3), false, &w("\n"))).0, "  ab\n  \ncd");
        assert_eq!(insert_line(&w(t), (3, 3), false, &w("\n")).sel, (7, 7));
        assert_eq!(apply(t, insert_line(&w(t), (3, 3), true, &w("\n"))).0, "  \n  ab\ncd");
        assert_eq!(insert_line(&w(t), (3, 3), true, &w("\n")).sel, (2, 2));
    }

    #[test]
    fn enter_and_home() {
        let t = w("x\n\t  ab");
        assert_eq!(enter_indent(&t, 7), &w("\t  ")[..]);
        assert_eq!(enter_indent(&t, 3), &w("\t")[..]);
        assert!(enter_indent(&t, 1).is_empty());
        assert_eq!(smart_home(&t, 7), 5);
        assert_eq!(smart_home(&t, 5), 2);
        assert_eq!(smart_home(&t, 3), 5);
    }

    #[test]
    fn indent_unit_follows_the_file() {
        assert_eq!(indent_unit(&w("a\n\n    b")), w("    "));
        assert_eq!(indent_unit(&w("a\r\n\tb")), w("\t"));
        assert_eq!(indent_unit(&w("a\nb")), w("\t"));
    }

    #[test]
    fn indent_and_outdent_lines() {
        let t = "a\n\nb";
        let e = indent_lines(&w(t), (0, 4), &w("\t"), false).unwrap();
        assert_eq!(apply(t, e), ("\ta\n\n\tb".into(), "\ta\n\n\tb".into()));
        let t = "\ta\n  b\nc";
        let e = indent_lines(&w(t), (1, 6), &w("\t"), true).unwrap();
        assert_eq!(apply(t, e), ("a\nb\nc".into(), "a\nb".into()));
        assert_eq!(indent_lines(&w("a\nb"), (0, 3), &w("\t"), true), None);
    }

    #[test]
    fn args_quoted_and_unquoted() {
        assert_eq!(parse_args(r#""C:\x\np.exe" "C:\my notes.txt""#).file.as_deref(), Some(r"C:\my notes.txt"));
        assert_eq!(parse_args(r#"np.exe my notes.txt"#).file.as_deref(), Some("my notes.txt"));
        assert_eq!(parse_args(r#"np.exe"#).file, None);
        let a = parse_args(r#"np.exe /A /p "f.txt""#);
        assert!(a.force_ansi && a.print && a.file.as_deref() == Some("f.txt"));
        // a path starting with /a... is not a switch
        assert_eq!(parse_args("np.exe /apple.txt").file.as_deref(), Some("/apple.txt"));
    }

    #[test]
    fn import_pins_switch() {
        let a = parse_args(r#""C:\x\RENotepad.exe" /ImportNotepadPins"#);
        assert!(a.import_pins && a.file.is_none());
        assert!(parse_args("np.exe /importnotepadpins").import_pins);
        // only as a whole word: a file name that starts with it stays a file
        let a = parse_args("np.exe /ImportNotepadPins.txt");
        assert!(!a.import_pins && a.file.as_deref() == Some("/ImportNotepadPins.txt"));
    }

    #[test]
    fn pin_switch_takes_the_file() {
        let a = parse_args(r#""C:\x\RENotepad.exe" /PinToJumpList "C:\my notes.txt""#);
        assert!(a.pin && a.file.as_deref() == Some(r"C:\my notes.txt"));
        assert!(!parse_args("np.exe /PinToJumpListX.txt").pin);
    }
}
