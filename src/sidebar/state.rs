use crate::config::CockpitConfig;

use super::sessions::SessionList;
use super::tracker::UsageTracker;

/// Which panel is displayed in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarPanel {
    Output,
    Sessions,
    Usage,
}

/// Complete sidebar state.
pub struct SidebarState {
    pub visible: bool,
    pub width: u32,
    pub panel: SidebarPanel,
    pub usage: UsageTracker,
    pub sessions: SessionList,
}

impl SidebarState {
    pub fn new(config: &CockpitConfig) -> Self {
        Self {
            visible: config.sidebar_visible,
            width: config.sidebar_width,
            panel: SidebarPanel::Output,
            usage: UsageTracker::new(),
            sessions: SessionList::new(),
        }
    }

    pub fn toggle_visibility(&mut self) {
        self.visible = !self.visible;
    }

    pub fn switch_panel(&mut self, panel: SidebarPanel) {
        self.panel = panel;
    }
}
