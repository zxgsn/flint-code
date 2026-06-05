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
use unicode_width::UnicodeWidthStr;

// ── Types ──────────────────────────────────────────────────────────────────

pub enum InputResult {
    Line(String),
    Exit,
}

// ── History ────────────────────────────────────────────────────────────────

struct HistoryState {
    lines: Vec<String>,
    index: Option<usize>,
}

static HISTORY: Mutex<HistoryState> = Mutex::new(HistoryState {
    lines: Vec::new(),
    index: None,
});

fn add_to_history(line: &str) {
    if !line.trim().is_empty() {
        let mut state = HISTORY.lock().unwrap();
        // Remove duplicate if exists
        state.lines.retain(|h| h != line);
        state.lines.push(line.to_string());
        state.index = None;
    }
}

fn get_prev_history() -> Option<String> {
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
    ("/clear", "Clear session"),
    ("/status", "Show status"),
    ("/help", "Show help"),
    ("/quit", "Exit"),
];

// ── Public entry point ─────────────────────────────────────────────────────

/// Read a line from the terminal using crossterm raw mode.
pub fn read_line() -> Result<InputResult> {
    enable_raw_mode()?;
    let result = read_line_inner();
    disable_raw_mode()?;
    println!();
    result
}

// ── Core input loop ────────────────────────────────────────────────────────

fn read_line_inner() -> Result<InputResult> {
    let mut buf = String::new();
    let mut ctrl_c_count: u8 = 0;
    let mut completion_lines: u16 = 0;
    let mut tab_index: usize = 0;

    // Record the input row once at the start
    let (_, input_row) = crossterm::cursor::position()?;

    loop {
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
                    clear_used_lines(input_row, completion_lines)?;
                    let mut stdout = std::io::stdout();
                    execute!(stdout, crossterm::cursor::MoveTo(0, input_row))?;
                    execute!(
                        stdout,
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
                    )?;
                    write!(stdout, "(press Ctrl+C again to exit)")?;
                    stdout.flush()?;
                    buf.clear();
                    completion_lines = 0;
                }
                // Tab — cycle through matching completions
                (KeyCode::Tab, _) => {
                    if buf.starts_with('/') {
                        let matches = get_completions(&buf);
                        if !matches.is_empty() {
                            tab_index = tab_index % matches.len();
                            buf = matches[tab_index].to_string();
                            tab_index += 1;
                            render_input_and_completions(&buf, &mut completion_lines, input_row)?;
                        }
                    }
                    ctrl_c_count = 0;
                }
                // Enter — submit
                (KeyCode::Enter, _) => {
                    clear_used_lines(input_row, completion_lines)?;
                    let mut stdout = std::io::stdout();
                    execute!(stdout, crossterm::cursor::MoveTo(0, input_row))?;
                    execute!(
                        stdout,
                        crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
                    )?;
                    write!(stdout, "\u{276f} {}", buf)?;
                    stdout.flush()?;
                    add_to_history(&buf);
                    return Ok(InputResult::Line(buf));
                }
                // Up arrow — previous history
                (KeyCode::Up, _) => {
                    if let Some(prev) = get_prev_history() {
                        buf = prev;
                        tab_index = 0;
                        render_input_and_completions(&buf, &mut completion_lines, input_row)?;
                    }
                    ctrl_c_count = 0;
                }
                // Down arrow — next history
                (KeyCode::Down, _) => {
                    if let Some(next) = get_next_history() {
                        buf = next;
                        tab_index = 0;
                        render_input_and_completions(&buf, &mut completion_lines, input_row)?;
                    }
                    ctrl_c_count = 0;
                }
                // Backspace
                (KeyCode::Backspace, _) => {
                    if buf.pop().is_some() {
                        tab_index = 0;
                        render_input_and_completions(&buf, &mut completion_lines, input_row)?;
                    }
                    ctrl_c_count = 0;
                }
                // Regular character
                (KeyCode::Char(c), m) if m.is_empty() || m == KeyModifiers::SHIFT => {
                    buf.push(c);
                    tab_index = 0;
                    render_input_and_completions(&buf, &mut completion_lines, input_row)?;
                    ctrl_c_count = 0;
                }
                (KeyCode::Esc, _) => {
                    ctrl_c_count = 0;
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

fn clear_used_lines(input_row: u16, old_completion_lines: u16) -> Result<()> {
    let mut stdout = std::io::stdout();
    let total = 1 + old_completion_lines;
    for i in 0..total {
        execute!(
            stdout,
            crossterm::cursor::MoveTo(0, input_row + i),
            crossterm::terminal::Clear(crossterm::terminal::ClearType::CurrentLine)
        )?;
    }
    stdout.flush()?;
    Ok(())
}

fn render_input_and_completions(
    buf: &str,
    completion_lines: &mut u16,
    input_row: u16,
) -> Result<()> {
    let mut stdout = std::io::stdout();
    let prompt = "\u{276f} ";
    let term_w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80) as usize;

    // 1. Get completions for ghost text
    let matches = get_completions(buf);
    let ghost = if buf.starts_with('/') && !buf.is_empty() {
        matches.first().and_then(|cmd| {
            if cmd.len() > buf.len() {
                Some(&cmd[buf.len()..])
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

    // 2. Clear old completion lines by overwriting with spaces
    for i in 0..*completion_lines {
        execute!(
            stdout,
            crossterm::cursor::MoveTo(0, input_row + 1 + i)
        )?;
        let pad = " ".repeat(term_w);
        write!(stdout, "{}", pad)?;
    }

    // 3. Draw input line — overwrite with padded content
    execute!(stdout, crossterm::cursor::MoveTo(0, input_row))?;
    let line_content = format!("{}{}", prompt, buf);
    let ghost_text = ghost.unwrap_or("");
    let full_line = format!("{}{}", line_content, ghost_text);
    let padded = if full_line.len() < term_w {
        format!("{:<width$}", full_line, width = term_w)
    } else {
        full_line.clone()
    };
    write!(stdout, "{}", padded)?;

    // 4. Redraw with proper colors (overwrite the padded version)
    execute!(stdout, crossterm::cursor::MoveTo(0, input_row))?;
    write!(stdout, "{}{}", prompt, buf)?;
    if let Some(ghost_text) = ghost {
        execute!(
            stdout,
            crossterm::style::SetForegroundColor(crossterm::style::Color::DarkGrey)
        )?;
        write!(stdout, "{}", ghost_text)?;
        execute!(stdout, crossterm::style::ResetColor)?;
    }

    // 5. Draw completion list below input
    if has_completions {
        for (i, cmd) in matches.iter().enumerate() {
            let desc = SLASH_COMMANDS
                .iter()
                .find(|(c, _)| *c == *cmd)
                .map(|(_, d)| *d)
                .unwrap_or("");
            execute!(
                stdout,
                crossterm::cursor::MoveTo(0, input_row + 1 + i as u16)
            )?;
            write!(stdout, "  {:<12} {}", cmd, desc)?;
        }
        *completion_lines = new_completion_lines;
    } else {
        *completion_lines = 0;
    }

    // 6. Move cursor to input position
    let cursor_col = prompt.width() as u16 + buf.width() as u16;
    execute!(stdout, crossterm::cursor::MoveTo(cursor_col, input_row))?;
    stdout.flush()?;

    Ok(())
}
