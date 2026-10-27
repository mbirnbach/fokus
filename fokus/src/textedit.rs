//! Editing text in place on the canvas: a caret and selection over the
//! text's characters, moved and edited by keys.

/// Caret (`cursor`) and the other end of the selection (`anchor`), as
/// character indices into the whole text (lines joined with '\n').
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caret {
    pub cursor: usize,
    pub anchor: usize,
}

impl Caret {
    pub fn at(i: usize) -> Self {
        Caret { cursor: i, anchor: i }
    }

    pub fn range(&self) -> (usize, usize) {
        (self.cursor.min(self.anchor), self.cursor.max(self.anchor))
    }

    pub fn has_selection(&self) -> bool {
        self.cursor != self.anchor
    }

    /// Move the caret; with `extend` the anchor stays (shift held).
    pub fn move_to(&mut self, i: usize, extend: bool) {
        self.cursor = i;
        if !extend {
            self.anchor = i;
        }
    }
}

fn byte(text: &str, ch: usize) -> usize {
    text.char_indices().nth(ch).map_or(text.len(), |(b, _)| b)
}

pub fn char_count(text: &str) -> usize {
    text.chars().count()
}

/// Replace the selection with `s`; the caret ends after it.
pub fn insert(text: &mut String, caret: &mut Caret, s: &str) {
    let (a, b) = caret.range();
    text.replace_range(byte(text, a)..byte(text, b), s);
    *caret = Caret::at(a + char_count(s));
}

pub fn selected(text: &str, caret: &Caret) -> String {
    let (a, b) = caret.range();
    text[byte(text, a)..byte(text, b)].to_string()
}

/// Backspace (`forward` false) or Delete.
pub fn delete(text: &mut String, caret: &mut Caret, forward: bool) {
    if !caret.has_selection() {
        let n = char_count(text);
        if forward && caret.cursor < n {
            caret.cursor += 1;
        } else if !forward && caret.cursor > 0 {
            caret.cursor -= 1;
        } else {
            return;
        }
    }
    insert(text, caret, "");
}

/// (line, column) of a character index.
pub fn line_col(text: &str, i: usize) -> (usize, usize) {
    let mut line = 0;
    let mut col = 0;
    for c in text.chars().take(i) {
        if c == '\n' {
            line += 1;
            col = 0;
        } else {
            col += 1;
        }
    }
    (line, col)
}

/// Character index of (line, column); the column is clamped to the line.
pub fn index(text: &str, line: usize, col: usize) -> usize {
    let mut i = 0;
    for (k, l) in text.split('\n').enumerate() {
        let len = char_count(l);
        if k == line {
            return i + col.min(len);
        }
        i += len + 1;
    }
    char_count(text)
}

/// Start of the word before `i` (Alt+Left), or end of the word after it.
pub fn word_boundary(text: &str, i: usize, forward: bool) -> usize {
    let chars: Vec<char> = text.chars().collect();
    let word = |c: char| c.is_alphanumeric();
    let mut j = i;
    if forward {
        while j < chars.len() && !word(chars[j]) {
            j += 1;
        }
        while j < chars.len() && word(chars[j]) {
            j += 1;
        }
    } else {
        while j > 0 && !word(chars[j - 1]) {
            j -= 1;
        }
        while j > 0 && word(chars[j - 1]) {
            j -= 1;
        }
    }
    j
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_and_deleting() {
        let mut t = String::from("Hllo");
        let mut c = Caret::at(1);
        insert(&mut t, &mut c, "e");
        assert_eq!((t.as_str(), c.cursor), ("Hello", 2));
        c.move_to(5, false);
        c.move_to(0, true);
        insert(&mut t, &mut c, "Grüß");
        assert_eq!(t, "Grüß");
        delete(&mut t, &mut c, false);
        assert_eq!(t, "Grü");
        c = Caret::at(0);
        delete(&mut t, &mut c, false);
        assert_eq!(t, "Grü");
        delete(&mut t, &mut c, true);
        assert_eq!(t, "rü");
    }

    #[test]
    fn lines_and_words() {
        let t = "ab\ncde\n";
        assert_eq!(line_col(t, 4), (1, 1));
        assert_eq!(index(t, 1, 9), 6);
        assert_eq!(index(t, 2, 0), 7);
        assert_eq!(selected(t, &Caret { cursor: 1, anchor: 4 }), "b\nc");
        let w = "laser cut it";
        assert_eq!(word_boundary(w, 0, true), 5);
        assert_eq!(word_boundary(w, 9, false), 6);
    }
}
