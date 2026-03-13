use tokio::sync::mpsc;

use claude_kernel::{ClaudeMessage, Session, SessionOptions};

/// Commands sent to the session task.
pub enum SessionCommand {
    SendMessage(String),
    Interrupt,
    Kill,
}

/// Events received from the session task.
pub enum SessionEvent {
    Message(Box<ClaudeMessage>),
    RateLimit {
        utilization: f64,
        status: String,
        resets_at: i64,
    },
    ContextUpdate {
        context_tokens: u64,
    },
    Error(String),
    Finished {
        cost_usd: Option<f64>,
        duration_ms: Option<u64>,
    },
}

/// Manages a Claude kernel session on a background tokio task.
pub struct SessionManager {
    cmd_tx: mpsc::Sender<SessionCommand>,
    event_rx: mpsc::Receiver<SessionEvent>,
    session_id: Option<String>,
}

impl SessionManager {
    /// Spawn a new Claude session with the given prompt.
    pub fn spawn(
        prompt: String,
        options: SessionOptions,
        rt: &tokio::runtime::Runtime,
    ) -> crate::error::Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<SessionCommand>(32);
        let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(256);

        rt.spawn(Self::session_loop(prompt, options, cmd_rx, event_tx));

        Ok(Self {
            cmd_tx,
            event_rx,
            session_id: None,
        })
    }

    /// Spawn a warm session (no initial prompt).
    pub fn warmup(
        options: SessionOptions,
        rt: &tokio::runtime::Runtime,
    ) -> crate::error::Result<Self> {
        let (cmd_tx, cmd_rx) = mpsc::channel::<SessionCommand>(32);
        let (event_tx, event_rx) = mpsc::channel::<SessionEvent>(256);

        rt.spawn(Self::warmup_loop(options, cmd_rx, event_tx));

        Ok(Self {
            cmd_tx,
            event_rx,
            session_id: None,
        })
    }

    /// Send a command to the session task.
    pub fn send(&self, cmd: SessionCommand) -> crate::error::Result<()> {
        self.cmd_tx
            .try_send(cmd)
            .map_err(|e| crate::error::CockpitError::Session(format!("send failed: {e}")))
    }

    /// Non-blocking receive of the next session event.
    pub fn try_recv(&mut self) -> Option<SessionEvent> {
        match self.event_rx.try_recv() {
            Ok(event) => {
                // Capture session_id from messages
                if let SessionEvent::Message(ref msg) = event {
                    if let Some(id) = msg.session_id() {
                        self.session_id = Some(id.to_string());
                    }
                }
                Some(event)
            }
            Err(_) => None,
        }
    }

    pub fn session_id(&self) -> Option<&str> {
        self.session_id.as_deref()
    }

    async fn session_loop(
        prompt: String,
        options: SessionOptions,
        mut cmd_rx: mpsc::Receiver<SessionCommand>,
        event_tx: mpsc::Sender<SessionEvent>,
    ) {
        let session = Session::new(&prompt, options).await;
        let mut session = match session {
            Ok(s) => s,
            Err(e) => {
                let _ = event_tx
                    .send(SessionEvent::Error(format!("spawn failed: {e}")))
                    .await;
                return;
            }
        };

        Self::run_session(&mut session, &mut cmd_rx, &event_tx).await;
    }

    async fn warmup_loop(
        options: SessionOptions,
        mut cmd_rx: mpsc::Receiver<SessionCommand>,
        event_tx: mpsc::Sender<SessionEvent>,
    ) {
        let session = Session::warmup(options).await;
        let mut session = match session {
            Ok(s) => s,
            Err(e) => {
                let _ = event_tx
                    .send(SessionEvent::Error(format!("warmup failed: {e}")))
                    .await;
                return;
            }
        };

        Self::run_session(&mut session, &mut cmd_rx, &event_tx).await;
    }

    async fn run_session(
        session: &mut Session,
        cmd_rx: &mut mpsc::Receiver<SessionCommand>,
        event_tx: &mpsc::Sender<SessionEvent>,
    ) {
        loop {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(SessionCommand::SendMessage(msg)) => {
                            if let Err(e) = session.inject_user_message(&msg).await {
                                let _ = event_tx
                                    .send(SessionEvent::Error(format!("inject failed: {e}")))
                                    .await;
                            }
                        }
                        Some(SessionCommand::Interrupt) => {
                            if let Err(e) = session.interrupt() {
                                let _ = event_tx
                                    .send(SessionEvent::Error(format!("interrupt failed: {e}")))
                                    .await;
                            }
                        }
                        Some(SessionCommand::Kill) => {
                            let _ = session.kill().await;
                            return;
                        }
                        None => return,
                    }
                }
                msg = session.next_message() => {
                    match msg {
                        Ok(Some(claude_msg)) => {
                            // Extract rate limit events
                            if let ClaudeMessage::RateLimitEvent(ref rl) = claude_msg {
                                let _ = event_tx
                                    .send(SessionEvent::RateLimit {
                                        utilization: rl.rate_limit_info.utilization,
                                        status: rl.rate_limit_info.status.clone(),
                                        resets_at: rl.rate_limit_info.resets_at,
                                    })
                                    .await;
                            }
                            // Extract context update from usage
                            if let Some(usage) = claude_msg.usage() {
                                let _ = event_tx
                                    .send(SessionEvent::ContextUpdate {
                                        context_tokens: usage.total_context(),
                                    })
                                    .await;
                            }
                            // Extract result info
                            if let ClaudeMessage::Result(ref result) = claude_msg {
                                let _ = event_tx
                                    .send(SessionEvent::Finished {
                                        cost_usd: result.cost_usd,
                                        duration_ms: result.duration_ms,
                                    })
                                    .await;
                            }
                            let _ = event_tx.send(SessionEvent::Message(Box::new(claude_msg))).await;
                        }
                        Ok(None) => return,
                        Err(e) => {
                            let _ = event_tx
                                .send(SessionEvent::Error(format!("stream error: {e}")))
                                .await;
                            return;
                        }
                    }
                }
            }
        }
    }
}
