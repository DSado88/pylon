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

    let mut results = Vec::with_capacity(requests.len());

    for req in requests {
        let session = match find_claude_child(req.shell_pid) {
            Some((claude_pid, session_id)) => {
                let cwd = get_process_cwd(claude_pid);
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
            }
            None => None,
        };

        results.push(TabScanResult {
            shell_pid: req.shell_pid,
            session,
        });
    }

    results
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
