/// Status of a Claude session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    Finished,
    Error,
}

/// Summary of a single Claude session for display in the sidebar.
pub struct SessionSummary {
    pub session_id: String,
    pub status: SessionStatus,
    pub turn_count: u32,
    pub total_cost_usd: f64,
}

/// Ordered list of sessions with an active selection.
pub struct SessionList {
    sessions: Vec<SessionSummary>,
    active_index: Option<usize>,
}

impl SessionList {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            active_index: None,
        }
    }

    pub fn add(&mut self, summary: SessionSummary) {
        self.active_index = Some(self.sessions.len());
        self.sessions.push(summary);
    }

    pub fn update(&mut self, session_id: &str, status: SessionStatus) {
        for summary in &mut self.sessions {
            if summary.session_id == session_id {
                summary.status = status;
                break;
            }
        }
    }

    pub fn active(&self) -> Option<&SessionSummary> {
        self.active_index
            .and_then(|i| self.sessions.get(i))
    }

    pub fn iter(&self) -> std::slice::Iter<'_, SessionSummary> {
        self.sessions.iter()
    }
}

impl Default for SessionList {
    fn default() -> Self {
        Self::new()
    }
}
