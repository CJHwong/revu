//! A multi-line text box: typing, a selection made with shift and the arrows, copy and
//! cut, and a paste that replaces the selection. Positions are byte offsets into the
//! text, always on a character boundary.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Editor {
    pub text: String,
    /// Where typing goes.
    pub cursor: usize,
    /// The other end of the selection, while there is one.
    pub anchor: Option<usize>,
}

/// What a key asks the owner of the box to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The key was handled inside the box.
    Handled,
    /// Put this text on the clipboard.
    Copy(String),
    /// ctrl+s: send the text.
    Submit,
    /// esc: close the box.
    Cancel,
}

impl Editor {
    /// The selected range, start first, when it is not empty.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let anchor = self.anchor?;
        let range = (anchor.min(self.cursor), anchor.max(self.cursor));
        (range.0 != range.1).then_some(range)
    }

    /// Typed or pasted text replaces the selection.
    pub fn insert(&mut self, text: &str) {
        self.delete_selection();
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.text.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    pub fn key(&mut self, key: KeyEvent) -> Outcome {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let word = key
            .modifiers
            .intersects(KeyModifiers::ALT | KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return Outcome::Cancel,
            KeyCode::Char('s') if control => return Outcome::Submit,
            KeyCode::Char('a') if control => {
                self.anchor = Some(0);
                self.cursor = self.text.len();
            }
            KeyCode::Char('c') if control => return self.copy(false),
            KeyCode::Char('x') if control => return self.copy(true),
            KeyCode::Char(_) if control => {}
            KeyCode::Char(character) => self.insert(character.encode_utf8(&mut [0; 4])),
            KeyCode::Enter => self.insert("\n"),
            KeyCode::Backspace => self.erase(|editor| editor.previous(editor.cursor)),
            KeyCode::Delete => self.erase(|editor| editor.next(editor.cursor)),
            KeyCode::Left if word => self.move_to(self.word_start(), shift),
            KeyCode::Right if word => self.move_to(self.word_end(), shift),
            KeyCode::Left => self.move_to(self.previous(self.cursor), shift),
            KeyCode::Right => self.move_to(self.next(self.cursor), shift),
            KeyCode::Up => self.move_to(self.vertical(-1), shift),
            KeyCode::Down => self.move_to(self.vertical(1), shift),
            KeyCode::Home => self.move_to(self.line_start(self.cursor), shift),
            KeyCode::End => self.move_to(self.line_end(self.cursor), shift),
            _ => {}
        }
        Outcome::Handled
    }

    /// Copies the selection, or every line when nothing is selected; a cut also removes it.
    fn copy(&mut self, cut: bool) -> Outcome {
        let Some((start, end)) = self.selection() else {
            return Outcome::Copy(self.text.clone());
        };
        let text = self.text[start..end].to_string();
        if cut {
            self.delete_selection();
        }
        Outcome::Copy(text)
    }

    /// Backspace and delete remove the selection, or one character beside the cursor.
    fn erase(&mut self, other_end: impl Fn(&Editor) -> usize) {
        if self.delete_selection() {
            return;
        }
        let other = other_end(self);
        let (start, end) = (other.min(self.cursor), other.max(self.cursor));
        self.text.replace_range(start..end, "");
        self.cursor = start;
    }

    fn delete_selection(&mut self) -> bool {
        let selection = self.selection();
        self.anchor = None;
        let Some((start, end)) = selection else {
            return false;
        };
        self.text.replace_range(start..end, "");
        self.cursor = start;
        true
    }

    /// Moves the cursor; with shift the selection grows from where it was.
    fn move_to(&mut self, position: usize, extend: bool) {
        match extend {
            true => {
                self.anchor.get_or_insert(self.cursor);
            }
            false => self.anchor = None,
        }
        self.cursor = position;
    }

    fn previous(&self, position: usize) -> usize {
        self.text[..position]
            .char_indices()
            .next_back()
            .map_or(0, |(index, _)| index)
    }

    fn next(&self, position: usize) -> usize {
        self.text[position..]
            .chars()
            .next()
            .map_or(position, |character| position + character.len_utf8())
    }

    fn line_start(&self, position: usize) -> usize {
        self.text[..position]
            .rfind('\n')
            .map_or(0, |index| index + 1)
    }

    fn line_end(&self, position: usize) -> usize {
        self.text[position..]
            .find('\n')
            .map_or(self.text.len(), |index| position + index)
    }

    /// The same column on the line above or below, or the line's end when it is shorter.
    fn vertical(&self, direction: i32) -> usize {
        let start = self.line_start(self.cursor);
        let column = self.text[start..self.cursor].chars().count();
        let target_start = match direction {
            -1 if start == 0 => return 0,
            -1 => self.line_start(start - 1),
            _ => {
                let end = self.line_end(self.cursor);
                if end == self.text.len() {
                    return end;
                }
                end + 1
            }
        };
        let target_end = self.line_end(target_start);
        let line = &self.text[target_start..target_end];
        target_start + line.chars().take(column).map(char::len_utf8).sum::<usize>()
    }

    /// The start of the word before the cursor, skipping the spaces in between.
    fn word_start(&self) -> usize {
        let before = &self.text[..self.cursor];
        let trimmed = before.trim_end_matches(|character: char| !character.is_alphanumeric());
        trimmed
            .rfind(|character: char| !character.is_alphanumeric())
            .map_or(0, |index| {
                index + trimmed[index..].chars().next().map_or(1, char::len_utf8)
            })
    }

    /// The end of the word after the cursor, skipping the spaces in between.
    fn word_end(&self) -> usize {
        let after = &self.text[self.cursor..];
        let skipped = after.len()
            - after
                .trim_start_matches(|character: char| !character.is_alphanumeric())
                .len();
        let rest = &after[skipped..];
        let word = rest
            .find(|character: char| !character.is_alphanumeric())
            .unwrap_or(rest.len());
        self.cursor + skipped + word
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(editor: &mut Editor, code: KeyCode, modifiers: KeyModifiers) -> Outcome {
        editor.key(KeyEvent::new(code, modifiers))
    }

    fn typed(text: &str) -> Editor {
        let mut editor = Editor::default();
        for character in text.chars() {
            press(&mut editor, KeyCode::Char(character), KeyModifiers::NONE);
        }
        editor
    }

    #[test]
    fn typing_enter_and_backspace_edit_the_text() {
        let mut editor = typed("héllo");
        press(&mut editor, KeyCode::Enter, KeyModifiers::NONE);
        press(&mut editor, KeyCode::Char('x'), KeyModifiers::NONE);
        press(&mut editor, KeyCode::Backspace, KeyModifiers::NONE);
        press(&mut editor, KeyCode::Left, KeyModifiers::NONE);
        press(&mut editor, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(editor.text, "héll\n");
    }

    #[test]
    fn shift_selects_and_ctrl_c_copies_it() {
        let mut editor = typed("one two");
        for _ in 0..3 {
            press(&mut editor, KeyCode::Left, KeyModifiers::SHIFT);
        }
        let copied = press(&mut editor, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(copied, Outcome::Copy("two".to_string()));
        assert_eq!(editor.text, "one two", "a copy keeps the text");
    }

    #[test]
    fn a_paste_or_a_typed_key_replaces_the_selection() {
        let mut editor = typed("keep old");
        press(
            &mut editor,
            KeyCode::Left,
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        );
        editor.insert("new\r\nline");
        assert_eq!(editor.text, "keep new\nline");
        press(&mut editor, KeyCode::Char('a'), KeyModifiers::CONTROL);
        press(&mut editor, KeyCode::Char('z'), KeyModifiers::NONE);
        assert_eq!(editor.text, "z");
    }

    #[test]
    fn cut_removes_the_selection_and_copy_without_one_takes_everything() {
        let mut editor = typed("abc");
        assert_eq!(
            press(&mut editor, KeyCode::Char('c'), KeyModifiers::CONTROL),
            Outcome::Copy("abc".to_string())
        );
        press(&mut editor, KeyCode::Home, KeyModifiers::SHIFT);
        press(&mut editor, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert_eq!(editor.text, "");
    }

    #[test]
    fn up_and_down_keep_the_column_or_stop_at_a_short_line() {
        let mut editor = typed("long line\nab\nanother");
        press(&mut editor, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(editor.cursor, "long line\nab".len(), "the short line's end");
        press(&mut editor, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(editor.cursor, 2, "column 2 of the first line");
        press(&mut editor, KeyCode::Down, KeyModifiers::NONE);
        press(&mut editor, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(editor.cursor, "long line\nab\nan".len());
    }

    #[test]
    fn word_moves_skip_spaces_and_punctuation() {
        let mut editor = typed("fn  call(arg)");
        press(&mut editor, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(&editor.text[editor.cursor..], "arg)");
        press(&mut editor, KeyCode::Left, KeyModifiers::ALT);
        assert_eq!(&editor.text[editor.cursor..], "call(arg)");
        press(&mut editor, KeyCode::Right, KeyModifiers::ALT);
        assert_eq!(&editor.text[editor.cursor..], "(arg)");
    }

    #[test]
    fn esc_cancels_and_ctrl_s_submits() {
        let mut editor = typed("x");
        assert_eq!(
            press(&mut editor, KeyCode::Esc, KeyModifiers::NONE),
            Outcome::Cancel
        );
        assert_eq!(
            press(&mut editor, KeyCode::Char('s'), KeyModifiers::CONTROL),
            Outcome::Submit
        );
    }
}
