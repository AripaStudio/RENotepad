//! Per-tab undo/redo. The edit control's own undo is one step that toggles,
//! so each change is instead diffed against a copy of the text.

/// `removed` was replaced by `inserted` at `at`.
struct Step {
    at: usize,
    removed: Vec<u16>,
    inserted: Vec<u16>,
    /// A key typed over a selection: the keys after it join it, as typing does.
    typed: bool,
}

/// What undo/redo does to the control: `start..end` becomes `text`, then `sel` is selected.
pub struct Change {
    pub start: usize,
    pub end: usize,
    pub text: Vec<u16>,
    pub sel: (usize, usize),
}

pub struct History {
    text: Vec<u16>,
    undo: Vec<Step>,
    redo: Vec<Step>,
    /// After an undo/redo the next edit starts its own step.
    sealed: bool,
    /// `undo.len()` when the file was last saved; None once that state can't come back.
    saved: Option<usize>,
}

fn is_space(c: u16) -> bool {
    matches!(c, 0x20 | 0x09 | 0x0D | 0x0A)
}

/// One keystroke's worth: a character, or Enter's CRLF.
fn is_key(t: &[u16]) -> bool {
    t.len() == 1 || t == [0x0D, 0x0A]
}

impl History {
    pub fn new(text: Vec<u16>) -> Self {
        History { text, undo: Vec::new(), redo: Vec::new(), sealed: false, saved: Some(0) }
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn mark_saved(&mut self) {
        self.saved = Some(self.undo.len());
    }

    /// The text is back to what was last saved.
    pub fn is_saved(&self) -> bool {
        self.saved == Some(self.undo.len())
    }

    /// The control now holds `now` with the caret at `caret`. The text after
    /// the caret is untouched by any edit, which settles where the change was
    /// when the diff alone is ambiguous (typing "b" into "a|b").
    pub fn record(&mut self, now: Vec<u16>, caret: usize) {
        let old = &self.text;
        let max = old.len().min(now.len());
        let mut tail = 0;
        while tail < max.min(now.len().saturating_sub(caret)) && old[old.len() - 1 - tail] == now[now.len() - 1 - tail] {
            tail += 1;
        }
        let mut head = 0;
        while head < max - tail && old[head] == now[head] {
            head += 1;
        }
        if head + tail == old.len() && head + tail == now.len() {
            return;
        }
        let (removed, inserted) = (old[head..old.len() - tail].to_vec(), now[head..now.len() - tail].to_vec());
        let typed = !removed.is_empty() && is_key(&inserted);
        let step = Step { at: head, removed, inserted, typed };
        self.text = now;
        self.redo.clear();
        if self.saved.is_some_and(|n| n > self.undo.len()) {
            self.saved = None;
        }
        // Growing the saved step would make undoing to it skip past the saved text.
        let sealed = std::mem::take(&mut self.sealed) || self.is_saved();
        if !sealed && self.merge(&step) {
            return;
        }
        self.undo.push(step);
    }

    /// Folds typing into the previous step until a new word starts, and runs of Backspace/Delete together.
    fn merge(&mut self, step: &Step) -> bool {
        let Some(last) = self.undo.last_mut() else { return false };
        if (last.removed.is_empty() || last.typed) && step.removed.is_empty() && is_key(&step.inserted) {
            let new_word = last.inserted.last().is_some_and(|&c| is_space(c)) && !is_space(step.inserted[0]);
            if step.at != last.at + last.inserted.len() || new_word {
                return false;
            }
            last.inserted.extend_from_slice(&step.inserted);
            return true;
        }
        if last.inserted.is_empty() && step.inserted.is_empty() && is_key(&step.removed) {
            if step.at + step.removed.len() == last.at {
                last.at = step.at;
                last.removed.splice(0..0, step.removed.iter().copied());
                return true;
            }
            if step.at == last.at {
                last.removed.extend_from_slice(&step.removed);
                return true;
            }
        }
        false
    }

    pub fn undo(&mut self) -> Option<Change> {
        let step = self.undo.pop()?;
        let end = step.at + step.inserted.len();
        self.text.splice(step.at..end, step.removed.iter().copied());
        let change = Change { start: step.at, end, text: step.removed.clone(), sel: (step.at, step.at + step.removed.len()) };
        self.redo.push(step);
        self.sealed = true;
        Some(change)
    }

    pub fn redo(&mut self) -> Option<Change> {
        let step = self.redo.pop()?;
        let end = step.at + step.removed.len();
        self.text.splice(step.at..end, step.inserted.iter().copied());
        let caret = step.at + step.inserted.len();
        let change = Change { start: step.at, end, text: step.inserted.clone(), sel: (caret, caret) };
        self.undo.push(step);
        self.sealed = true;
        Some(change)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    /// Types `s` one character at a time at the end.
    fn type_in(h: &mut History, s: &str) {
        for c in s.chars() {
            let mut t = h.text.clone();
            t.push(c as u16);
            let n = t.len();
            h.record(t, n);
        }
    }

    fn apply(text: &mut Vec<u16>, c: &Change) {
        text.splice(c.start..c.end, c.text.iter().copied());
    }

    #[test]
    fn typing_undoes_a_word_at_a_time() {
        let mut h = History::new(Vec::new());
        type_in(&mut h, "hello world");
        let mut t = h.text.clone();
        apply(&mut t, &h.undo().unwrap());
        assert_eq!(t, w("hello "));
        apply(&mut t, &h.undo().unwrap());
        assert_eq!(t, w(""));
        assert!(h.undo().is_none());
    }

    #[test]
    fn redo_restores_and_new_edit_drops_it() {
        let mut h = History::new(Vec::new());
        type_in(&mut h, "ab cd");
        let mut t = h.text.clone();
        apply(&mut t, &h.undo().unwrap());
        apply(&mut t, &h.redo().unwrap());
        assert_eq!(t, w("ab cd"));
        apply(&mut t, &h.undo().unwrap());
        type_in(&mut h, "x");
        assert!(!h.can_redo());
        assert_eq!(h.text, w("ab x"));
    }

    #[test]
    fn caret_places_ambiguous_insert() {
        let mut h = History::new(w("ab"));
        h.record(w("abb"), 2);
        h.record(w("abbb"), 3);
        let mut t = h.text.clone();
        let c = h.undo().unwrap();
        assert_eq!((c.start, c.end), (1, 3));
        apply(&mut t, &c);
        assert_eq!(t, w("ab"));
    }

    #[test]
    fn backspaces_merge() {
        let mut h = History::new(w("abc"));
        h.record(w("ab"), 2);
        h.record(w("a"), 1);
        let mut t = h.text.clone();
        let c = h.undo().unwrap();
        assert_eq!(c.sel, (1, 3));
        apply(&mut t, &c);
        assert_eq!(t, w("abc"));
    }

    #[test]
    fn undo_to_save_point_is_saved() {
        let mut h = History::new(Vec::new());
        type_in(&mut h, "ab");
        h.mark_saved();
        type_in(&mut h, "c");
        assert!(!h.is_saved());
        h.undo();
        assert!(h.is_saved());
        h.undo();
        assert!(!h.is_saved());
        h.redo();
        assert!(h.is_saved());
        h.undo();
        type_in(&mut h, "x");
        assert!(!h.is_saved());
        h.undo();
        assert!(!h.is_saved());
    }

    #[test]
    fn typing_over_a_selection_is_one_step() {
        let mut h = History::new(w("a bc d"));
        h.record(w("a x d"), 3);
        h.record(w("a xy d"), 4);
        let mut t = h.text.clone();
        let c = h.undo().unwrap();
        assert_eq!(c.sel, (2, 4));
        apply(&mut t, &c);
        assert_eq!(t, w("a bc d"));
        assert!(!h.can_undo());
    }

    #[test]
    fn unchanged_text_records_nothing() {
        let mut h = History::new(w("abc"));
        h.record(w("abc"), 1);
        assert!(!h.can_undo());
    }
}
