use chrono::Offset;
use std::process::Command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeStatus {
    Working,
    Idle,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ClaudeSession {
    pub pid: u32,
    pub session_id: String,
    pub status: ClaudeStatus,
    pub context_pct: u8,
    pub topic: String,
    pub git_branch: String,
    pub project: String,
    pub cwd: String,
}

/// Find a `claude` process that is a direct child of the given shell PID.
/// Returns `(claude_pid, session_id)` if found.
pub fn find_claude_child(shell_pid: u32) -> Option<(u32, String)> {
    let output = Command::new("pgrep")
        .args(["-P", &shell_pid.to_string(), "-x", "claude"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let pid: u32 = stdout.lines().next()?.trim().parse().ok()?;

    let session_id = extract_session_id(pid)?;
    Some((pid, session_id))
}

/// Extract the session UUID for a running claude process.
/// Tries: (1) `--resume <UUID>` from command line,
/// (2) JSONL whose birth time best matches the process start time.
pub fn extract_session_id(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;

    let cmd = String::from_utf8_lossy(&output.stdout).to_string();

    if let Some(uuid) = extract_resume_uuid(&cmd) {
        return Some(uuid);
    }

    let start_time = get_process_start_time(pid)?;
    session_id_from_birthtime(pid, start_time)
}

/// Get a process's start time as a SystemTime via `ps -o lstart=`.
fn get_process_start_time(pid: u32) -> Option<std::time::SystemTime> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let text = stdout.trim();
    if text.is_empty() {
        return None;
    }

    // Format: "Wed Mar 13 12:46:30 2026"
    // Parse with chrono
    let dt = chrono::NaiveDateTime::parse_from_str(text, "%a %b %e %H:%M:%S %Y").ok()?;
    let local = chrono::Local::now().offset().fix();
    let dt_local = dt.and_local_timezone(local).single()?;
    Some(std::time::SystemTime::from(dt_local))
}

/// Parse `--resume <UUID>` out of a command string.
pub fn extract_resume_uuid(command: &str) -> Option<String> {
    let idx = command.find("--resume ")?;
    let rest = command.get(idx + 9..)?;
    let uuid_str = rest.get(..36)?;
    if is_uuid(uuid_str) {
        Some(uuid_str.to_string())
    } else {
        None
    }
}

/// Get the current working directory for a process via `lsof`.
pub fn get_process_cwd(pid: u32) -> String {
    let output = match Command::new("lsof")
        .args(["-p", &pid.to_string(), "-Fn"])
        .output()
    {
        Ok(o) => o,
        Err(_) => return String::new(),
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut found_cwd = false;

    for line in stdout.lines() {
        if line == "fcwd" {
            found_cwd = true;
            continue;
        }
        if found_cwd {
            if let Some(path) = line.strip_prefix("n") {
                return path.to_string();
            }
        }
    }

    String::new()
}

/// Validate that a string is a UUID in 8-4-4-4-12 hex-with-dashes format.
pub fn is_uuid(s: &str) -> bool {
    if s.len() != 36 {
        return false;
    }
    s.bytes().enumerate().all(|(i, b)| {
        if i == 8 || i == 13 || i == 18 || i == 23 {
            b == b'-'
        } else {
            b.is_ascii_hexdigit()
        }
    })
}

/// Find the JSONL whose birth time (creation time) is closest to the Claude
/// process start time. This uniquely identifies sessions even when multiple
/// Claude processes share the same CWD.
fn session_id_from_birthtime(pid: u32, proc_start: std::time::SystemTime) -> Option<String> {
    let cwd = get_process_cwd(pid);
    if cwd.is_empty() {
        return None;
    }

    let home = std::env::var("HOME").unwrap_or_default();
    let encoded_cwd = cwd.replace('/', "-");
    let project_dir = format!("{home}/.claude/projects/{encoded_cwd}");
    let project_path = std::path::Path::new(&project_dir);

    if !project_path.exists() {
        return None;
    }

    let entries = std::fs::read_dir(project_path).ok()?;

    let mut best: Option<(std::time::Duration, String)> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        if ext != Some("jsonl") {
            continue;
        }

        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s,
            None => continue,
        };

        if !is_uuid(stem) {
            continue;
        }

        // Use birth time (creation time) — available on macOS via .created()
        let birth = match entry.metadata().and_then(|m| m.created()) {
            Ok(t) => t,
            Err(_) => continue,
        };

        // Consider files created near the process start time, within a
        // [-10s, +300s] window. Claude may not create the JSONL until the user
        // sends the first message (up to 5 min), and startup overhead can make
        // the file appear slightly before the recorded proc_start.
        let delta = match birth.duration_since(proc_start) {
            Ok(d) => d,
            Err(_) => {
                // birth < proc_start — allow up to 10 seconds of startup overhead
                match proc_start.duration_since(birth) {
                    Ok(d) if d.as_secs() <= 10 => d,
                    _ => continue,
                }
            }
        };

        if delta.as_secs() > 300 {
            continue; // too far from start — not from this process
        }

        match &best {
            Some((prev_delta, _)) if delta < *prev_delta => {
                best = Some((delta, stem.to_string()));
            }
            None => {
                best = Some((delta, stem.to_string()));
            }
            _ => {}
        }
    }

    best.map(|(_, id)| id)
}
