//! The cursor of a one-line text box, such as ctrl+l or the label filter.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A move or a delete that macOS keys ask for in a text box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextKey {
    LineStart,
    LineEnd,
    WordLeft,
    WordRight,
    DeleteToLineStart,
    DeleteWordBack,
}

/// Cmd and Option with an arrow or Backspace, in each form a terminal sends them.
/// Ghostty and iTerm2 send readline keys: Cmd+Left as ctrl+a, Cmd+Right as ctrl+e,
/// Cmd+Backspace as ctrl+u, and Option+Left and Right as esc b and esc f. A terminal
/// that reports modifiers sends the arrow with Super or Alt.
pub fn text_key(key: &KeyEvent) -> Option<TextKey> {
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let command = key.modifiers.contains(KeyModifiers::SUPER);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let text_key = match key.code {
        KeyCode::Char('a') if control && !shift => TextKey::LineStart,
        KeyCode::Char('e') if control => TextKey::LineEnd,
        KeyCode::Char('u') if control => TextKey::DeleteToLineStart,
        KeyCode::Char('b') if alt => TextKey::WordLeft,
        KeyCode::Char('f') if alt => TextKey::WordRight,
        KeyCode::Left if command => TextKey::LineStart,
        KeyCode::Right if command => TextKey::LineEnd,
        KeyCode::Backspace if command => TextKey::DeleteToLineStart,
        KeyCode::Left if alt => TextKey::WordLeft,
        KeyCode::Right if alt => TextKey::WordRight,
        KeyCode::Backspace if alt => TextKey::DeleteWordBack,
        _ => return None,
    };
    Some(text_key)
}

/// Where a word move from `position` lands, in characters: back to the start of the
/// word before, or on to the end of the word after, skipping what is not a word between.
pub fn word_edge(chars: &[char], position: usize, forward: bool) -> usize {
    let in_word = |index: usize| chars[index].is_alphanumeric();
    let mut at = position;
    if forward {
        while at < chars.len() && !in_word(at) {
            at += 1;
        }
        while at < chars.len() && in_word(at) {
            at += 1;
        }
        return at;
    }
    while at > 0 && !in_word(at - 1) {
        at -= 1;
    }
    while at > 0 && in_word(at - 1) {
        at -= 1;
    }
    at
}

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
    pub fn key(&mut self, text: &mut String, key: KeyEvent) -> bool {
        if let Some(text_key) = text_key(&key) {
            self.text_key(text, text_key);
            return true;
        }
        match key.code {
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

    fn text_key(&mut self, text: &mut String, text_key: TextKey) {
        let chars: Vec<char> = text.chars().collect();
        let at = chars.len().saturating_sub(self.after);
        let start = match text_key {
            TextKey::LineStart => return self.after = chars.len(),
            TextKey::LineEnd => return self.after = 0,
            TextKey::WordLeft => return self.after = chars.len() - word_edge(&chars, at, false),
            TextKey::WordRight => return self.after = chars.len() - word_edge(&chars, at, true),
            TextKey::DeleteToLineStart => 0,
            TextKey::DeleteWordBack => word_edge(&chars, at, false),
        };
        *text = chars[..start].iter().chain(&chars[at..]).collect();
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
            assert!(
                cursor.key(text, KeyEvent::from(*key)),
                "{key:?} edits or moves"
            );
        }
    }

    fn chord(cursor: &mut LineCursor, text: &mut String, code: KeyCode, modifiers: KeyModifiers) {
        assert!(
            cursor.key(text, KeyEvent::new(code, modifiers)),
            "{code:?} {modifiers:?}"
        );
    }

    /// Before and after the cursor, joined by a bar.
    fn shown(cursor: LineCursor, text: &str) -> String {
        let (before, after) = text.split_at(cursor.byte(text));
        format!("{before}|{after}")
    }

    #[test]
    fn cmd_and_option_keys_move_and_delete_as_the_terminal_sends_them() {
        use KeyCode::{Backspace, Char, Left, Right};
        let (control, alt, cmd) = (
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SUPER,
        );
        let mut text = "acme/app#12 fix".to_string();
        let mut cursor = LineCursor::default();
        // Ghostty and iTerm2 send Cmd+Left as ctrl+a and Option+Left as esc b.
        chord(&mut cursor, &mut text, Char('b'), alt);
        assert_eq!(shown(cursor, &text), "acme/app#12 |fix");
        chord(&mut cursor, &mut text, Left, alt);
        assert_eq!(shown(cursor, &text), "acme/app#|12 fix");
        chord(&mut cursor, &mut text, Char('a'), control);
        assert_eq!(shown(cursor, &text), "|acme/app#12 fix");
        chord(&mut cursor, &mut text, Char('f'), alt);
        assert_eq!(shown(cursor, &text), "acme|/app#12 fix");
        chord(&mut cursor, &mut text, Right, alt);
        assert_eq!(shown(cursor, &text), "acme/app|#12 fix");
        chord(&mut cursor, &mut text, Char('e'), control);
        assert_eq!(shown(cursor, &text), "acme/app#12 fix|");
        chord(&mut cursor, &mut text, Left, cmd);
        assert_eq!(shown(cursor, &text), "|acme/app#12 fix");
        chord(&mut cursor, &mut text, Right, cmd);
        assert_eq!(shown(cursor, &text), "acme/app#12 fix|");
        // Option+Backspace deletes a word, Cmd+Backspace (ctrl+u) the text before.
        chord(&mut cursor, &mut text, Backspace, alt);
        assert_eq!(shown(cursor, &text), "acme/app#12 |");
        chord(&mut cursor, &mut text, Left, KeyModifiers::NONE);
        chord(&mut cursor, &mut text, Char('u'), control);
        assert_eq!(shown(cursor, &text), "| ");
        text = "ab cd".to_string();
        cursor = LineCursor::default();
        chord(&mut cursor, &mut text, Backspace, cmd);
        assert_eq!(shown(cursor, &text), "|");
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
        assert!(!cursor.key(&mut text, KeyEvent::from(KeyCode::Enter)));
    }

    #[test]
    fn a_text_shorter_than_the_count_puts_the_cursor_first() {
        let mut cursor = LineCursor::default();
        let mut text = "abc".to_string();
        cursor.key(&mut text, KeyEvent::from(KeyCode::Home));
        assert_eq!(cursor.byte("x"), 0);
    }
}
