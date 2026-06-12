//! REPL input handling with crossterm raw mode.
//!
//! Provides line editing, Tab completion for slash commands, and Ctrl+C handling.

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use std::io::Write;
use std::sync::Mutex;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

// ── Types ──────────────────────────────────────────────────────────────────

pub enum InputResult {
    Line(String),
    Exit,
}

// ── History (persistent) ───────────────────────────────────────────────────

/// Maximum number of history entries to keep on disk.
const MAX_HISTORY: usize = 1000;

struct HistoryState {
    lines: Vec<String>,
    index: Option<usize>,
    loaded: bool,
}

static HISTORY: Mutex<HistoryState> = Mutex::new(HistoryState {
    lines: Vec::new(),
    index: None,
    loaded: false,
});

/// Path to the history file: `~/.flint/history`
fn history_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".flint").join("history"))
}

/// Load history from disk on first access.
fn ensure_loaded() {
    let mut state = HISTORY.lock().unwrap();
    if state.loaded {
        return;
    }
    state.loaded = true;

    if let Some(path) = history_path() {
        if let Ok(content) = std::fs::read_to_string(&path) {
            state.lines = content
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect();
            // Trim to MAX_HISTORY from the front (keep most recent)
            if state.lines.len() > MAX_HISTORY {
                let drain = state.lines.len() - MAX_HISTORY;
                state.lines.drain(..drain);
            }
        }
    }
}

/// Append a single line to the history file.
fn append_to_file(line: &str) {
    if let Some(path) = history_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // Read existing, append, trim, write back
        let mut lines: Vec<String> = std::fs::read_to_string(&path)
            .ok()
            .map(|c| c.lines().filter(|l| !l.is_empty()).map(String::from).collect())
            .unwrap_or_default();

        // Remove duplicate
        lines.retain(|h| h != line);
        lines.push(line.to_string());

        // Trim
        if lines.len() > MAX_HISTORY {
            let drain = lines.len() - MAX_HISTORY;
            lines.drain(..drain);
        }

        let _ = std::fs::write(path, lines.join("\n"));
    }
}

fn add_to_history(line: &str) {
    if !line.trim().is_empty() {
        ensure_loaded();
        {
            let mut state = HISTORY.lock().unwrap();
            state.lines.retain(|h| h != line);
            state.lines.push(line.to_string());
            state.index = None;
        }
        append_to_file(line);
    }
}

fn get_prev_history() -> Option<String> {
    ensure_loaded();
    let mut state = HISTORY.lock().unwrap();
    if state.lines.is_empty() {
        return None;
    }

    let idx = match state.index {
        Some(i) => {
            if i == 0 {
                return None;
            }
            i - 1
        }
        None => state.lines.len() - 1,
    };

    state.index = Some(idx);
    Some(state.lines[idx].clone())
}

fn get_next_history() -> Option<String> {
    ensure_loaded();
    let mut state = HISTORY.lock().unwrap();
    match state.index {
        Some(i) => {
            if i >= state.lines.len() - 1 {
                state.index = None;
                Some(String::new())
            } else {
                state.index = Some(i + 1);
                Some(state.lines[i + 1].clone())
            }
        }
        None => None,
    }
}

// ── Slash command definitions ──────────────────────────────────────────────

pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/config", "Open settings panel"),
    ("/setup", "Configure provider"),
    ("/model", "Switch model"),
    ("/skills", "List skills"),
    ("/mcp", "MCP server status"),
    ("/memory", "Memory status"),
    ("/resume", "Restore saved session"),
    ("/compact", "Compress history"),
    ("/clear", "Clear session"),
    ("/status", "Show status"),
    ("/swarm", "Swarm status / spawn terminal agent"),
    ("/poke", "Toggle auto-poke"),
    ("/undo", "Revert last turn's file changes"),
    ("/help", "Show help"),
    ("/quit", "Exit"),
];

// ── Path completion ────────────────────────────────────────────────────────

/// Extract the partial path token at the cursor position for completion.
/// Returns (prefix_before_path, partial_path) if the cursor is on a path-like token.
fn extract_path_token(buf: &str, cursor_pos: usize) -> Option<(String, String)> {
    // Find the start of the current "word" (space-delimited, but include path separators)
    let before = &buf[..cursor_pos];
    let word_start = before.rfind(|c: char| c == ' ' || c == '\t').map(|i| i + 1).unwrap_or(0);
    let word = &buf[word_start..cursor_pos];

    if word.is_empty() {
        return None;
    }

    // Check if it looks like a path: contains / or \ or starts with . or ~
    let looks_like_path = word.contains('/') || word.contains('\\')
        || word.starts_with('.') || word.starts_with('~');

    if !looks_like_path {
        return None;
    }

    Some((buf[..word_start].to_string(), word.to_string()))
}

/// Get filesystem path completions for a partial path.
fn get_path_completions(partial: &str, working_dir: &std::path::Path) -> Vec<String> {
    let (dir_part, file_prefix) = if let Some(pos) = partial.rfind(|c: char| c == '/' || c == '\\') {
        (&partial[..pos + 1], &partial[pos + 1..])
    } else {
        (".", partial)
    };

    let search_dir = if dir_part.starts_with('~') {
        if let Some(home) = dirs::home_dir() {
            home.join(&dir_part[2..]) // strip "~/"
        } else {
            return Vec::new();
        }
    } else if dir_part.starts_with('.') || dir_part.starts_with('/') || dir_part.len() > 1 && dir_part.as_bytes()[1] == b':' {
        std::path::PathBuf::from(dir_part)
    } else {
        working_dir.join(dir_part)
    };

    let mut results = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&search_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(file_prefix) && !name.starts_with('.') {
                let path = entry.path();
                let display = if dir_part == "." {
                    name.clone()
                } else {
                    format!("{}{}", dir_part, name)
                };
                if path.is_dir() {
                    results.push(format!("{}/", display));
                } else {
                    results.push(display);
                }
            }
        }
    }

    results.sort();
    results
}

// ── Public entry point ─────────────────────────────────────────────────────

/// Read a line from the terminal using crossterm raw mode.
pub fn read_line() -> Result<InputResult> {
    enable_raw_mode()?;
    let result = read_line_inner(|| {});
    disable_raw_mode()?;
    println!();
    result
}

/// Read a line with a periodic handler that runs during poll timeouts.
/// The handler is called every ~100ms while waiting for user input.
/// This allows processing input requests from sub-agents without blocking.
pub fn read_line_with_handler<F: FnMut()>(handler: F) -> Result<InputResult> {
    enable_raw_mode()?;
    let result = read_line_inner(handler);
    disable_raw_mode()?;
    println!();
    result
}

// ── Core input loop ────────────────────────────────────────────────────────

fn read_line_inner<F: FnMut()>(mut handler: F) -> Result<InputResult> {
    let mut buf = String::new();
    let mut cursor_pos: usize = 0;
    let mut ctrl_c_count: u8 = 0;
    let mut completion_lines: u16 = 0;
    let mut tab_index: usize = 0;
    let mut undo_stack: Vec<(String, usize)> = Vec::new();
    // Paste detection: count consecutive rapid characters.
    // Normal typing has pauses (arrows, tab, etc. reset this).
    // Pasting sends many chars without interruption.
    let mut rapid_char_count: u32 = 0;

    // Record the input row once at the start
    let (_, mut start_row) = crossterm::cursor::position()?;

    loop {
        // Poll with 100ms timeout instead of blocking event::read().
        // This lets us run the handler periodically to process input requests.
        if !event::poll(std::time::Duration::from_millis(100))? {
            // No event — run the handler
            handler();
            continue;
        }
        if let Event::Key(KeyEvent {
            code, modifiers, kind, ..
        }) = event::read()?
        {
            if kind != event::KeyEventKind::Press {
                continue;
            }
            match (code, modifiers) {
                // Ctrl+C — double press to exit
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    ctrl_c_count += 1;
                    if ctrl_c_count >= 2 {
                        return Ok(InputResult::Exit);
                    }
                    clear_used_lines(start_row, completion_lines)?;
                    let mut stdout = std::io::stdout();
                    execute!(stdout, crossterm::cursor::MoveTo(0, start_row))?;
                    execute!(
                        stdout,
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
                    )?;
                    write!(stdout, "(press Ctrl+C again to exit)")?;
                    stdout.flush()?;
                    buf.clear();
                    cursor_pos = 0;
                    completion_lines = 0;
                }
                // Tab — cycle through matching completions (slash commands or paths)
                (KeyCode::Tab, _) => {
                    if buf.starts_with('/') {
                        // Slash command completion
                        let matches = get_completions(&buf);
                        if !matches.is_empty() {
                            tab_index = tab_index % matches.len();
                            buf = matches[tab_index].to_string();
                            cursor_pos = buf.len();
                            tab_index += 1;
                            render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                        }
                    } else if let Some((prefix, partial)) = extract_path_token(&buf, cursor_pos) {
                        // Path completion
                        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                        let matches = get_path_completions(&partial, &cwd);
                        if !matches.is_empty() {
                            tab_index = tab_index % matches.len();
                            let completed = format!("{}{}", prefix, matches[tab_index]);
                            buf = completed;
                            cursor_pos = buf.len();
                            tab_index += 1;
                            render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                        }
                    }
                    ctrl_c_count = 0;
                }
                // Enter — submit (or insert newline in paste mode)
                (KeyCode::Enter, _) => {
                    let is_paste = rapid_char_count > 10;
                    rapid_char_count = 0;
                    if is_paste {
                        // Pasted content with newlines: insert literally
                        undo_stack.push((buf.clone(), cursor_pos));
                        if !buf.is_char_boundary(cursor_pos) {
                            cursor_pos = buf.len();
                        }
                        buf.insert(cursor_pos, '\n');
                        cursor_pos += 1;
                        tab_index = 0;
                        render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    } else {
                        clear_used_lines(start_row, completion_lines)?;
                        let mut stdout = std::io::stdout();
                        execute!(stdout, crossterm::cursor::MoveTo(0, start_row))?;
                        execute!(
                            stdout,
                            crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
                        )?;
                        write!(stdout, "\u{276f} {}", buf)?;
                        stdout.flush()?;
                        add_to_history(&buf);
                        return Ok(InputResult::Line(buf));
                    }
                }
                // Up arrow — previous history
                (KeyCode::Up, _) => {
                    if let Some(prev) = get_prev_history() {
                        buf = prev;
                        cursor_pos = buf.len();
                        tab_index = 0;
                        render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    }
                    ctrl_c_count = 0;
                }
                // Down arrow — next history
                (KeyCode::Down, _) => {
                    if let Some(next) = get_next_history() {
                        buf = next;
                        cursor_pos = buf.len();
                        tab_index = 0;
                        render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    }
                    ctrl_c_count = 0;
                }
                // Left arrow — move cursor left
                (KeyCode::Left, _) => {
                    if cursor_pos > 0 {
                        // Find previous char boundary
                        cursor_pos = buf[..cursor_pos].char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
                        render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    }
                    ctrl_c_count = 0;
                }
                // Right arrow — move cursor right
                (KeyCode::Right, _) => {
                    if cursor_pos < buf.len() {
                        // Find next char boundary
                        cursor_pos = buf[cursor_pos..].char_indices().nth(1).map(|(i, _)| cursor_pos + i).unwrap_or(buf.len());
                        render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    }
                    ctrl_c_count = 0;
                }
                // Backspace
                (KeyCode::Backspace, _) => {
                    if cursor_pos > 0 {
                        undo_stack.push((buf.clone(), cursor_pos));
                        // Find previous char boundary
                        let prev_pos = buf[..cursor_pos].char_indices().next_back().map(|(i, _)| i).unwrap_or(0);
                        buf.drain(prev_pos..cursor_pos);
                        cursor_pos = prev_pos;
                        tab_index = 0;
                        render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    }
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                // Delete
                (KeyCode::Delete, _) => {
                    if cursor_pos < buf.len() {
                        undo_stack.push((buf.clone(), cursor_pos));
                        // Find next char boundary
                        let next_pos = buf[cursor_pos..].char_indices().nth(1).map(|(i, _)| cursor_pos + i).unwrap_or(buf.len());
                        buf.drain(cursor_pos..next_pos);
                        tab_index = 0;
                        render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    }
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                // Ctrl+A — move to beginning of line
                (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                    cursor_pos = 0;
                    render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                // Ctrl+E — move to end of line
                (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                    cursor_pos = buf.len();
                    render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                // Ctrl+U — delete to beginning of line
                (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                    undo_stack.push((buf.clone(), cursor_pos));
                    buf.drain(..cursor_pos);
                    cursor_pos = 0;
                    tab_index = 0;
                    render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                // Ctrl+K — delete to end of line
                (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                    undo_stack.push((buf.clone(), cursor_pos));
                    buf.truncate(cursor_pos);
                    tab_index = 0;
                    render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                // Ctrl+W — delete previous word
                (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                    if cursor_pos > 0 {
                        undo_stack.push((buf.clone(), cursor_pos));
                        let before = buf[..cursor_pos].to_string();
                        let new_pos = before.trim_end().len();
                        let trimmed = before[..new_pos].trim_end_matches(|c: char| !c.is_alphanumeric());
                        let new_cursor = trimmed.len();
                        buf.drain(new_cursor..cursor_pos);
                        cursor_pos = new_cursor;
                        tab_index = 0;
                        render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    }
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                // Ctrl+L — clear screen
                (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                    let mut stdout = std::io::stdout();
                    execute!(stdout, crossterm::terminal::Clear(crossterm::terminal::ClearType::All))?;
                    execute!(stdout, crossterm::cursor::MoveTo(0, 0))?;
                    // Re-render input at top and update start_row
                    start_row = 0;
                    render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                // Ctrl+Z — undo
                (KeyCode::Char('z'), KeyModifiers::CONTROL) => {
                    if let Some((prev_buf, prev_pos)) = undo_stack.pop() {
                        buf = prev_buf;
                        cursor_pos = prev_pos;
                        tab_index = 0;
                        render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    }
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                // Regular character
                (KeyCode::Char(c), m) if m.is_empty() || m == KeyModifiers::SHIFT => {
                    rapid_char_count += 1;

                    // Ensure cursor_pos is at a valid char boundary
                    if !buf.is_char_boundary(cursor_pos) {
                        cursor_pos = buf.len();
                    }
                    buf.insert(cursor_pos, c);
                    cursor_pos += c.len_utf8();
                    tab_index = 0;
                    render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    ctrl_c_count = 0;
                }
                // Ctrl+J — insert newline (alternative to Shift+Enter)
                (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                    undo_stack.push((buf.clone(), cursor_pos));
                    // Ensure cursor_pos is at a valid char boundary
                    if !buf.is_char_boundary(cursor_pos) {
                        cursor_pos = buf.len();
                    }
                    buf.insert(cursor_pos, '\n');
                    cursor_pos += 1;
                    tab_index = 0;
                    render_input_and_completions(&buf, cursor_pos, &mut completion_lines, start_row)?;
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                (KeyCode::Esc, _) => {
                    ctrl_c_count = 0;
                    rapid_char_count = 0;
                }
                _ => {}
            }
        }
    }
}

// ── Completions ────────────────────────────────────────────────────────────

fn get_completions(input: &str) -> Vec<&'static str> {
    if !input.starts_with('/') {
        return Vec::new();
    }
    SLASH_COMMANDS
        .iter()
        .filter(|(cmd, _)| cmd.starts_with(input))
        .map(|(cmd, _)| *cmd)
        .collect()
}

// ── Terminal rendering ─────────────────────────────────────────────────────

fn clear_used_lines(start_row: u16, old_completion_lines: u16) -> Result<()> {
    let mut stdout = std::io::stdout();
    let total = 1 + old_completion_lines;
    for i in 0..total {
        execute!(
            stdout,
            crossterm::cursor::MoveTo(0, start_row + i),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
        )?;
    }
    stdout.flush()?;
    Ok(())
}

fn render_input_and_completions(
    buf: &str,
    cursor_pos: usize,
    completion_lines: &mut u16,
    start_row: u16,
) -> Result<()> {
    let mut stdout = std::io::stdout();
    let prompt = "\u{276f} ";
    let term_w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80) as usize;

    // 1. Get completions for ghost text
    let matches = get_completions(buf);
    let ghost = if buf.starts_with('/') && !buf.is_empty() {
        matches.first().and_then(|cmd| {
            let cmd_width = cmd.width();
            let buf_width = buf.width();
            if cmd_width > buf_width {
                // Get the remaining part of the command
                let mut remaining = String::new();
                let mut current_width = 0;
                for c in cmd.chars() {
                    if current_width >= buf_width {
                        remaining.push(c);
                    }
                    current_width += c.width().unwrap_or(1);
                }
                if remaining.is_empty() {
                    None
                } else {
                    Some(remaining)
                }
            } else {
                None
            }
        })
    } else {
        None
    };

    let has_completions = buf.starts_with('/') && !matches.is_empty();
    let new_completion_lines = if has_completions {
        matches.len() as u16
    } else {
        0
    };

    // Calculate how many rows the input spans (considering newlines and char widths)
    let prompt_width = prompt.width();
    let mut current_row = 0u16;
    let mut current_col = prompt_width;
    let mut input_rows = 1u16;

    for c in buf.chars() {
        if c == '\n' {
            current_row += 1;
            current_col = 0;
            input_rows = current_row + 1;
        } else {
            let char_width = c.width().unwrap_or(1);
            current_col += char_width;
            if current_col >= term_w {
                current_row += 1;
                current_col = char_width;
                input_rows = current_row + 1;
            }
        }
    }

    // 2. Clear old completion lines
    for i in 0..*completion_lines {
        execute!(
            stdout,
            crossterm::cursor::MoveTo(0, start_row + input_rows + i),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
        )?;
    }

    // 3. Clear input lines
    for row in 0..input_rows {
        execute!(
            stdout,
            crossterm::cursor::MoveTo(0, start_row + row),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
        )?;
    }

    // Move to start for drawing
    execute!(stdout, crossterm::cursor::MoveTo(0, start_row))?;

    // Draw prompt
    write!(stdout, "{}", prompt)?;

    // Syntax highlighting
    let mut in_quote = false;
    let mut quote_char = ' ';
    let is_command = buf.starts_with('/');

    for c in buf.chars() {
        if c == '\n' {
            execute!(stdout, crossterm::style::ResetColor)?;
            write!(stdout, "\n")?;
            continue;
        }

        if is_command {
            execute!(stdout, crossterm::style::SetForegroundColor(crossterm::style::Color::Cyan))?;
            write!(stdout, "{}", c)?;
        } else {
            if !in_quote && (c == '"' || c == '\'') {
                in_quote = true;
                quote_char = c;
                execute!(stdout, crossterm::style::SetForegroundColor(crossterm::style::Color::Yellow))?;
                write!(stdout, "{}", c)?;
            } else if in_quote && c == quote_char {
                write!(stdout, "{}", c)?;
                execute!(stdout, crossterm::style::ResetColor)?;
                in_quote = false;
            } else {
                if !in_quote {
                    execute!(stdout, crossterm::style::ResetColor)?;
                }
                write!(stdout, "{}", c)?;
            }
        }
    }
    execute!(stdout, crossterm::style::ResetColor)?;

    if let Some(ghost_text) = ghost {
        execute!(
            stdout,
            crossterm::style::SetForegroundColor(crossterm::style::Color::DarkGrey)
        )?;
        write!(stdout, "{}", ghost_text)?;
        execute!(stdout, crossterm::style::ResetColor)?;
    }

    // 4. Draw completion list below input
    if has_completions {
        for (i, cmd) in matches.iter().enumerate() {
            let desc = SLASH_COMMANDS
                .iter()
                .find(|(c, _)| *c == *cmd)
                .map(|(_, d)| *d)
                .unwrap_or("");
            execute!(
                stdout,
                crossterm::cursor::MoveTo(0, start_row + input_rows + i as u16)
            )?;
            write!(stdout, "  {:<12} {}", cmd, desc)?;
        }
        *completion_lines = new_completion_lines;
    } else {
        *completion_lines = 0;
    }

    // 5. Move cursor to correct position (considering newlines and wrapping)
    let mut cursor_row = 0u16;
    let mut cursor_col = prompt_width;
    // Use char_indices to properly handle multi-byte characters
    for (i, c) in buf.char_indices() {
        if i >= cursor_pos {
            break;
        }
        if c == '\n' {
            cursor_row += 1;
            cursor_col = 0;
        } else {
            let char_width = c.width().unwrap_or(1);
            cursor_col += char_width;
            if cursor_col >= term_w {
                cursor_row += 1;
                cursor_col = char_width;
            }
        }
    }
    execute!(stdout, crossterm::cursor::MoveTo(cursor_col as u16, start_row + cursor_row))?;
    stdout.flush()?;

    Ok(())
}
