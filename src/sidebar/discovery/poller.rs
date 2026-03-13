use std::time::Duration;

use tokio::sync::{mpsc, watch};

use super::jsonl;
use super::tab_session::{find_claude_child, get_process_cwd, ClaudeSession, ClaudeStatus};

const ACTIVE_THRESHOLD_SECS: f64 = 30.0;

#[derive(Debug, Clone)]
pub struct TabScanRequest {
    pub shell_pid: u32,
}

#[derive(Debug, Clone)]
pub struct TabScanResult {
    pub shell_pid: u32,
    pub session: Option<ClaudeSession>,
}

pub fn spawn(
    rt: &tokio::runtime::Handle,
    poll_interval: Duration,
) -> (
    watch::Sender<Vec<TabScanRequest>>,
    mpsc::UnboundedReceiver<Vec<TabScanResult>>,
) {
    let (req_tx, req_rx) = watch::channel::<Vec<TabScanRequest>>(Vec::new());
    let (res_tx, res_rx) = mpsc::unbounded_channel::<Vec<TabScanResult>>();

    rt.spawn(async move {
        loop {
            let requests = req_rx.borrow().clone();

            if !requests.is_empty() {
                let results = tokio::task::spawn_blocking(move || scan_tabs(&requests))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!("scan_tabs failed: {e}");
                        Vec::new()
                    });

                let _ = res_tx.send(results);
            }

            tokio::time::sleep(poll_interval).await;
        }
    });

    (req_tx, res_rx)
}

fn scan_tabs(requests: &[TabScanRequest]) -> Vec<TabScanResult> {
    let home = std::env::var("HOME").unwrap_or_default();
    let projects_dir = format!("{home}/.claude/projects");

    // Phase 1: Find Claude child processes and their session IDs.
    let mut raw: Vec<(u32, Option<(u32, String, String)>)> = Vec::with_capacity(requests.len());
    for req in requests {
        let found = find_claude_child(req.shell_pid).map(|(claude_pid, session_id)| {
            let cwd = get_process_cwd(claude_pid);
            (claude_pid, session_id, cwd)
        });
        raw.push((req.shell_pid, found));
    }

    // Phase 2: Detect duplicate session IDs (birthtime matching can cross-match
    // when multiple sessions share the same CWD). For duplicates, try to reassign
    // by finding unclaimed JSONLs in the project directory.
    let mut claimed_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut needs_reassign: Vec<usize> = Vec::new();

    for (i, (_, found)) in raw.iter().enumerate() {
        if let Some((_, ref sid, _)) = found {
            if !claimed_ids.insert(sid.clone()) {
                // Duplicate — mark for reassignment
                needs_reassign.push(i);
            }
        }
    }

    // Reassign duplicates: find unclaimed JSONLs by modification time
    for &i in &needs_reassign {
        if let Some((claude_pid, ref _old_sid, ref cwd)) = raw[i].1 {
            if let Some(new_sid) = find_unclaimed_jsonl(&projects_dir, cwd, &claimed_ids) {
                claimed_ids.insert(new_sid.clone());
                raw[i].1 = Some((claude_pid, new_sid, cwd.clone()));
            }
        }
    }

    // Phase 3: Build results with full session metadata.
    let mut results = Vec::with_capacity(requests.len());
    for (shell_pid, found) in raw {
        let session = found.and_then(|(claude_pid, session_id, cwd)| {
            let jsonl_path = find_jsonl(&projects_dir, &session_id, &cwd);

            let status = jsonl_path
                .as_ref()
                .and_then(|path| std::fs::metadata(path).ok())
                .and_then(|meta| meta.modified().ok())
                .and_then(|mtime| mtime.elapsed().ok())
                .map(|elapsed| {
                    if elapsed.as_secs_f64() < ACTIVE_THRESHOLD_SECS {
                        ClaudeStatus::Working
                    } else {
                        ClaudeStatus::Idle
                    }
                })
                .unwrap_or(ClaudeStatus::Idle);

            let meta = jsonl_path
                .as_deref()
                .map(|p| jsonl::extract_session_meta(p, 100))
                .unwrap_or_default();

            let project = cwd
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();

            Some(ClaudeSession {
                pid: claude_pid,
                session_id,
                status,
                context_pct: meta.context_pct,
                topic: meta.topic,
                git_branch: meta.git_branch,
                project,
                cwd,
            })
        });

        results.push(TabScanResult { shell_pid, session });
    }

    results
}

/// Find the most recently modified JSONL in a project dir that isn't already
/// claimed by another tab. Used to resolve duplicate session IDs.
fn find_unclaimed_jsonl(
    projects_dir: &str,
    cwd: &str,
    claimed: &std::collections::HashSet<String>,
) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    let project_dir_name = cwd.replace('/', "-");
    let dir = format!("{projects_dir}/{project_dir_name}");
    let path = std::path::Path::new(&dir);
    if !path.exists() {
        return None;
    }

    let mut candidates: Vec<(std::time::SystemTime, String)> = Vec::new();
    for entry in std::fs::read_dir(path).ok()?.flatten() {
        let p = entry.path();
        if p.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let stem = match p.file_stem().and_then(|s| s.to_str()) {
            Some(s) if s.len() == 36 => s,
            _ => continue,
        };
        if claimed.contains(stem) {
            continue; // already taken by another tab
        }
        if let Ok(meta) = entry.metadata() {
            if let Ok(mtime) = meta.modified() {
                candidates.push((mtime, stem.to_string()));
            }
        }
    }

    // Pick the most recently modified unclaimed JSONL
    candidates.sort_by(|a, b| b.0.cmp(&a.0));
    candidates.into_iter().next().map(|(_, id)| id)
}

fn find_jsonl(projects_dir: &str, session_id: &str, cwd: &str) -> Option<String> {
    // Try constructing path from CWD first (fast path)
    if !cwd.is_empty() {
        let project_dir_name = cwd.replace('/', "-");
        let direct_path = format!("{projects_dir}/{project_dir_name}/{session_id}.jsonl");
        if std::path::Path::new(&direct_path).exists() {
            return Some(direct_path);
        }
    }

    // Fallback: scan all project directories
    let projects_path = std::path::Path::new(projects_dir);
    if !projects_path.exists() {
        return None;
    }

    let entries = std::fs::read_dir(projects_path).ok()?;
    for entry in entries.flatten() {
        let session_path = entry.path().join(format!("{session_id}.jsonl"));
        if session_path.exists() {
            return session_path.to_str().map(|s| s.to_string());
        }
    }

    None
}
