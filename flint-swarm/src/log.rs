//! Log file management and viewer.
//!
//! Each sub-agent writes to a log file. Viewers can be opened
//! to tail logs in real-time.

use std::path::PathBuf;

pub fn log_dir() -> PathBuf {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".flint").join("swarm-logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

pub fn create_log(agent_id: &str, task_id: &str) -> (std::fs::File, PathBuf) {
    let path = log_dir().join(format!("{}_{}.log", agent_id, task_id));
    let file = std::fs::OpenOptions::new()
        .create(true).write(true).truncate(true).open(&path)
        .expect("failed to create log file");
    (file, path)
}

/// Open a dedicated viewer terminal for one agent's log.
pub fn open_agent_viewer(agent_id: &str, task_id: &str) {
    let log_path = log_dir().join(format!("{}_{}.log", agent_id, task_id));
    let path_str = log_path.to_string_lossy().to_string();
    let short = agent_id.strip_prefix("agent_").unwrap_or(agent_id);
    let title = format!("Agent [{}]", &short[..4.min(short.len())]);

    #[cfg(target_os = "windows")]
    {
        // Write a .ps1 script to avoid nested quote escaping issues
        let script_path = log_dir().join(format!("viewer_{}.ps1", &agent_id[agent_id.len()-4..]));
        let script = format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n\
             Write-Host '=== {title} ===' -ForegroundColor Cyan\n\
             Write-Host 'Waiting for log...' -ForegroundColor DarkGray\n\
             $found = $false\n\
             while (-not $found) {{\n\
               if ((Test-Path '{path}') -and ((Get-Item '{path}').Length -gt 0)) {{\n\
                 $found = $true\n\
                 Write-Host 'Log active, tailing...' -ForegroundColor Green\n\
                 Get-Content '{path}' -Wait -Tail 200 -Encoding UTF8\n\
               }}\n\
               if (-not $found) {{ Start-Sleep -Milliseconds 500 }}\n\
             }}\n\
             Write-Host ''\n\
             Write-Host '=== Agent finished. Log complete. ===' -ForegroundColor Yellow\n\
             Write-Host 'Press any key to close...' -ForegroundColor DarkGray\n\
             $null = $Host.UI.RawUI.ReadKey('NoEcho,IncludeKeyDown')",
            title = title,
            path = path_str.replace('\'', "''"),
        );
        let _ = std::fs::write(&script_path, script);
        let script_str = script_path.to_string_lossy().to_string();
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", &title, "powershell", "-ExecutionPolicy", "Bypass", "-NoExit", "-File", &script_str])
            .spawn();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let tail = format!(
            "echo '=== {} ===' && while [ ! -f '{}' ] || [ ! -s '{}' ]; do sleep 0.5; done && tail -f '{}'",
            title, path_str, path_str, path_str
        );
        if std::env::var("TMUX").is_ok() {
            let _ = std::process::Command::new("tmux")
                .args(["split-window", "-h", "-l", "40%", &tail]).status();
        } else {
            for term in &["xterm", "gnome-terminal", "konsole"] {
                let r = match *term {
                    "gnome-terminal" => std::process::Command::new(term)
                        .args(["--title", &title, "--", "bash", "-c", &tail]).spawn(),
                    _ => std::process::Command::new(term)
                        .args(["-e", &format!("bash -c '{}'", tail)]).spawn(),
                };
                if r.is_ok() { break; }
            }
        }
    }
}

/// Open an aggregated viewer for all logs.
pub fn open_viewer() {
    let dir = log_dir();
    let dir_str = dir.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    {
        let script_path = log_dir().join("viewer_all.ps1");
        let script = format!(
            "[Console]::OutputEncoding = [System.Text.Encoding]::UTF8\n\
             Write-Host '=== All Swarm Logs ===' -ForegroundColor Cyan\n\
             Write-Host 'Waiting for logs...' -ForegroundColor DarkGray\n\
             Set-Location '{dir}'\n\
             $found = $false\n\
             while (-not $found) {{\n\
               $logs = Get-ChildItem -Filter '*.log' -ErrorAction SilentlyContinue | Where-Object {{ $_.Length -gt 0 }}\n\
               if ($logs) {{\n\
                 $found = $true\n\
                 Write-Host 'Tailing all logs...' -ForegroundColor Green\n\
                 Get-Content $logs.FullName -Wait -Tail 20 -Encoding UTF8\n\
               }}\n\
               if (-not $found) {{ Start-Sleep -Seconds 1 }}\n\
             }}\n\
             Write-Host ''\n\
             Write-Host '=== All agents finished. ===' -ForegroundColor Yellow\n\
             Write-Host 'Press any key to close...' -ForegroundColor DarkGray\n\
             $null = $Host.UI.RawUI.ReadKey('NoEcho,IncludeKeyDown')",
            dir = dir_str.replace('\'', "''"),
        );
        let _ = std::fs::write(&script_path, script);
        let script_str = script_path.to_string_lossy().to_string();
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "Swarm Logs", "powershell", "-ExecutionPolicy", "Bypass", "-NoExit", "-File", &script_str])
            .spawn();
    }

    #[cfg(not(target_os = "windows"))]
    {
        let tail = format!("cd '{}' && tail -f *.log 2>/dev/null; sleep 999", dir_str);
        if std::env::var("TMUX").is_ok() {
            let _ = std::process::Command::new("tmux")
                .args(["split-window", "-h", "-l", "40%", &tail]).status();
        } else {
            for term in &["xterm", "gnome-terminal", "konsole"] {
                let r = match *term {
                    "gnome-terminal" => std::process::Command::new(term)
                        .args(["--", "bash", "-c", &tail]).spawn(),
                    _ => std::process::Command::new(term)
                        .args(["-e", &format!("bash -c '{}'", tail)]).spawn(),
                };
                if r.is_ok() { break; }
            }
        }
    }
}

pub fn viewer_mode_name() -> &'static str {
    #[cfg(target_os = "windows")] { "new terminal window" }
    #[cfg(not(target_os = "windows"))] {
        if std::env::var("TMUX").is_ok() { "tmux split-pane" } else { "new terminal window" }
    }
}

pub fn clean_logs() {
    let dir = log_dir();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if entry.path().extension().map_or(false, |e| e == "log") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}
