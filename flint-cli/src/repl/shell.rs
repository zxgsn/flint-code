//! Shell command execution (!command).

use std::path::Path;

/// Execute a shell command and print its output.
/// Returns true if the command was executed (even if it failed).
pub fn execute(cmd: &str, working_dir: &Path) -> bool {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        println!("Usage: !<command>  (e.g. !ls, !git status)\n");
        return true;
    }
    let (shell, flag, wrapped_cmd);
    if cfg!(target_os = "windows") {
        shell = "cmd";
        flag = "/C";
        wrapped_cmd = format!("chcp 65001 >nul && {}", cmd);
    } else {
        shell = "sh";
        flag = "-c";
        wrapped_cmd = cmd.to_string();
    }
    let output = std::process::Command::new(shell)
        .args([flag, &wrapped_cmd])
        .current_dir(working_dir)
        .output();
    match output {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            if !stdout.is_empty() {
                print!("{}", stdout);
            }
            if !stderr.is_empty() {
                eprint!("{}", stderr);
            }
            if !o.status.success() {
                eprintln!("[exit {}]", o.status.code().unwrap_or(-1));
            }
        }
        Err(e) => {
            eprintln!("Failed to run command: {}", e);
        }
    }
    println!();
    true
}
