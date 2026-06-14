//! Type-ahead input buffering for the REPL.
//!
//! Allows users to type while the agent is executing. Buffered input
//! is presented for review/editing after the agent completes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// Shared buffer holding keystrokes collected during agent execution.
#[derive(Debug)]
pub struct TypeaheadBuffer {
    /// The buffered characters.
    text: String,
    /// Byte offset of cursor within text.
    cursor: usize,
    /// Undo stack: (previous_text, previous_cursor).
    undo_stack: Vec<(String, usize)>,
    /// True if user pressed Enter while buffering.
    submitted: bool,
    /// True if user pressed Ctrl+C while buffering.
    cancelled: bool,
    /// Whether we've shown the "type-ahead active" notification.
    notified: bool,
}

impl TypeaheadBuffer {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            cursor: 0,
            undo_stack: Vec::new(),
            submitted: false,
            cancelled: false,
            notified: false,
        }
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn is_submitted(&self) -> bool {
        self.submitted
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled
    }

    pub fn is_notified(&self) -> bool {
        self.notified
    }

    pub fn set_notified(&mut self) {
        self.notified = true;
    }

    fn push_undo(&mut self) {
        self.undo_stack.push((self.text.clone(), self.cursor));
    }

    pub fn insert_char(&mut self, c: char) {
        self.push_undo();
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.push_undo();
            let prev = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.text.replace_range(prev..self.cursor, "");
            self.cursor = prev;
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.text.len() {
            self.push_undo();
            let next = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
            self.text.replace_range(self.cursor..next, "");
        }
    }

    pub fn move_left(&mut self) {
        if self.cursor > 0 {
            let prev = self.text[..self.cursor]
                .char_indices()
                .last()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.cursor = prev;
        }
    }

    pub fn move_right(&mut self) {
        if self.cursor < self.text.len() {
            let next = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(i, _)| self.cursor + i)
                .unwrap_or(self.text.len());
            self.cursor = next;
        }
    }

    pub fn move_home(&mut self) {
        self.cursor = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor = self.text.len();
    }

    pub fn delete_to_start(&mut self) {
        if self.cursor > 0 {
            self.push_undo();
            self.text.replace_range(..self.cursor, "");
            self.cursor = 0;
        }
    }

    pub fn delete_to_end(&mut self) {
        if self.cursor < self.text.len() {
            self.push_undo();
            self.text.truncate(self.cursor);
        }
    }

    pub fn delete_word(&mut self) {
        if self.cursor > 0 {
            self.push_undo();
            // Find start of previous word
            let before = &self.text[..self.cursor];
            let trimmed = before.trim_end();
            let new_cursor = if trimmed.len() < before.len() {
                // Was trailing whitespace, go to end of word before that
                trimmed
                    .rfind(|c: char| c.is_whitespace())
                    .map(|i| i + 1)
                    .unwrap_or(0)
            } else {
                // At end of word, delete to start of word
                before
                    .rfind(|c: char| c.is_whitespace())
                    .map(|i| i + 1)
                    .unwrap_or(0)
            };
            self.text.replace_range(new_cursor..self.cursor, "");
            self.cursor = new_cursor;
        }
    }

    pub fn undo(&mut self) {
        if let Some((prev_text, prev_cursor)) = self.undo_stack.pop() {
            self.text = prev_text;
            self.cursor = prev_cursor;
        }
    }

    pub fn insert_newline(&mut self) {
        self.push_undo();
        self.text.insert(self.cursor, '\n');
        self.cursor += 1;
    }

    pub fn submit(&mut self) {
        self.submitted = true;
    }

    pub fn cancel(&mut self) {
        self.cancelled = true;
    }
}

/// Spawn a background thread that reads crossterm events into the typeahead buffer.
///
/// Returns (join_handle, stop_flag). Set stop_flag to true to stop the thread.
pub fn spawn_typeahead_reader(
    buffer: Arc<Mutex<TypeaheadBuffer>>,
    cancel: Arc<AtomicBool>,
) -> (JoinHandle<()>, Arc<AtomicBool>) {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = stop.clone();

    let handle = std::thread::spawn(move || {
        // Enable raw mode for keystroke reading
        if crossterm::terminal::enable_raw_mode().is_err() {
            return;
        }

        loop {
            // Check stop flags
            if stop.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                break;
            }

            // Poll for events with 100ms timeout
            if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                match event::read() {
                    Ok(Event::Key(KeyEvent { code, modifiers, kind: KeyEventKind::Press, .. })) => {
                        let mut buf = buffer.lock().unwrap();

                        // Show notification on first keystroke
                        if !buf.is_notified() && !buf.is_submitted() && !buf.is_cancelled() {
                            buf.set_notified();
                            eprintln!("\x1b[90m  [type-ahead active -- editing will be available after agent finishes]\x1b[0m");
                        }

                        // Skip if already submitted or cancelled
                        if buf.is_submitted() || buf.is_cancelled() {
                            continue;
                        }

                        match (code, modifiers) {
                            // Ctrl+C: cancel agent and buffer
                            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                                buf.cancel();
                                cancel.store(true, Ordering::Relaxed);
                            }
                            // Enter: mark as submitted
                            (KeyCode::Enter, _) => {
                                buf.submit();
                            }
                            // Ctrl+J: insert newline
                            (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                                buf.insert_newline();
                            }
                            // Backspace
                            (KeyCode::Backspace, _) => {
                                buf.backspace();
                            }
                            // Delete
                            (KeyCode::Delete, _) => {
                                buf.delete();
                            }
                            // Left arrow
                            (KeyCode::Left, _) => {
                                buf.move_left();
                            }
                            // Right arrow
                            (KeyCode::Right, _) => {
                                buf.move_right();
                            }
                            // Home / Ctrl+A
                            (KeyCode::Home, _) | (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                                buf.move_home();
                            }
                            // End / Ctrl+E
                            (KeyCode::End, _) | (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                                buf.move_end();
                            }
                            // Ctrl+U: delete to start
                            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                                buf.delete_to_start();
                            }
                            // Ctrl+K: delete to end
                            (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                                buf.delete_to_end();
                            }
                            // Ctrl+W: delete word
                            (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                                buf.delete_word();
                            }
                            // Ctrl+Z: undo
                            (KeyCode::Char('z'), KeyModifiers::CONTROL) => {
                                buf.undo();
                            }
                            // Printable characters
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                buf.insert_char(c);
                            }
                            _ => {}
                        }
                    }
                    Ok(Event::Paste(text)) => {
                        let mut buf = buffer.lock().unwrap();
                        if !buf.is_submitted() && !buf.is_cancelled() {
                            // Show notification on first paste
                            if !buf.is_notified() {
                                buf.set_notified();
                                eprintln!("\x1b[90m  [type-ahead active -- editing will be available after agent finishes]\x1b[0m");
                            }
                            for c in text.chars() {
                                if c == '\n' || c == '\r' {
                                    buf.insert_newline();
                                } else {
                                    buf.insert_char(c);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }

        // Disable raw mode when done
        let _ = crossterm::terminal::disable_raw_mode();
    });

    (handle, stop_clone)
}
