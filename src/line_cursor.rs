//! The cursor of a one-line text box, such as ctrl+l or the label filter.

use crossterm::event::KeyCode;

/// Kept as how many characters sit after the cursor. Counting from the end leaves the
/// cursor last when code sets the text, which is where every box starts.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LineCursor {
    after: usize,
}

impl LineCursor {
    pub fn at_end(self) -> bool {
        self.after == 0
    }

    /// The cursor's byte index in `text`. A text shorter than the count puts it first.
    pub fn byte(self, text: &str) -> usize {
        let before = text.chars().count().saturating_sub(self.after);
        text.char_indices()
            .nth(before)
            .map_or(text.len(), |(index, _)| index)
    }

    pub fn insert(self, text: &mut String, typed: &str) {
        text.insert_str(self.byte(text), typed);
    }

    /// One key of a text box. False when the key is not one that edits or moves.
    pub fn key(&mut self, text: &mut String, key: KeyCode) -> bool {
        match key {
            KeyCode::Char(character) => self.insert(text, character.encode_utf8(&mut [0; 4])),
            KeyCode::Backspace => self.backspace(text),
            KeyCode::Delete => self.delete(text),
            KeyCode::Left => self.after = (self.after + 1).min(text.chars().count()),
            KeyCode::Right => self.after = self.after.saturating_sub(1),
            KeyCode::Home => self.after = text.chars().count(),
            KeyCode::End => self.after = 0,
            _ => return false,
        }
        true
    }

    fn backspace(self, text: &mut String) {
        let at = self.byte(text);
        if let Some((index, _)) = text[..at].char_indices().next_back() {
            text.remove(index);
        }
    }

    fn delete(&mut self, text: &mut String) {
        let at = self.byte(text);
        if at < text.len() {
            text.remove(at);
            self.after = self.after.saturating_sub(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(cursor: &mut LineCursor, text: &mut String, keys: &[KeyCode]) {
        for key in keys {
            assert!(cursor.key(text, *key), "{key:?} edits or moves");
        }
    }

    #[test]
    fn keys_move_and_edit_at_the_cursor() {
        use KeyCode::{Backspace, Char, Delete, End, Home, Left, Right};
        let mut text = String::new();
        let mut cursor = LineCursor::default();
        press(
            &mut cursor,
            &mut text,
            &[Char('a'), Char('c'), Left, Char('b')],
        );
        assert_eq!(text, "abc");
        press(&mut cursor, &mut text, &[Home, Delete, Right, Backspace]);
        assert_eq!(text, "c");
        assert_eq!(cursor.byte(&text), 0);
        press(&mut cursor, &mut text, &[End, Char('é'), Left, Left, Left]);
        assert_eq!(cursor.byte(&text), 0, "left stops at the start");
        press(&mut cursor, &mut text, &[Right, Right, Right]);
        assert!(cursor.at_end(), "right stops at the end");
        assert_eq!(cursor.byte(&text), "cé".len());
        assert!(!cursor.key(&mut text, KeyCode::Enter));
    }

    #[test]
    fn a_text_shorter_than_the_count_puts_the_cursor_first() {
        let mut cursor = LineCursor::default();
        let mut text = "abc".to_string();
        cursor.key(&mut text, KeyCode::Home);
        assert_eq!(cursor.byte("x"), 0);
    }
}
