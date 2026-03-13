use crate::config::CockpitConfig;

use super::discovery::ClaudeSession;
use super::usage::UsageData;

/// Which panel is displayed in the sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarPanel {
    Output,
    Sessions,
    Usage,
}

/// Per-account usage data.
#[derive(Debug, Clone)]
pub struct AccountUsage {
    pub account_name: String,
    pub data: UsageData,
}

/// One entry per cockpit tab in the sessions panel.
#[derive(Debug, Clone)]
pub struct TabSessionEntry {
    /// Index into App::windows -- for click-to-activate.
    pub tab_index: usize,
    /// Resolved display title (custom > topic > VT title > "Tab N").
    pub display_title: String,
    /// Claude session info if Claude is running in this tab.
    pub session: Option<ClaudeSession>,
}

/// Maps a range of sidebar rows to a tab entry (for click/hover hit-testing).
#[derive(Debug, Clone)]
pub struct SidebarHitEntry {
    pub start_row: u16,
    pub end_row: u16, // exclusive
    pub tab_index: usize,
}

/// Complete sidebar state.
pub struct SidebarState {
    pub visible: bool,
    pub width_px: u32,
    pub panel: SidebarPanel,
    pub accounts: Vec<AccountUsage>,
    pub usage_error: Option<String>,
    pub tab_entries: Vec<TabSessionEntry>,
    /// Row→tab mapping for click/hover in sessions panel.
    pub hit_map: Vec<SidebarHitEntry>,
    /// Which tab is currently hovered in the sidebar (for highlight).
    pub hovered_tab: Option<usize>,
    pub dirty: bool,
}

impl SidebarState {
    pub fn new(config: &CockpitConfig) -> Self {
        Self {
            visible: config.sidebar_visible,
            width_px: config.sidebar_width,
            panel: SidebarPanel::Sessions,
            accounts: Vec::new(),
            usage_error: None,
            tab_entries: Vec::new(),
            hit_map: Vec::new(),
            hovered_tab: None,
            dirty: true,
        }
    }

    pub fn toggle_visibility(&mut self) {
        self.visible = !self.visible;
        self.dirty = true;
    }

    pub fn switch_panel(&mut self, panel: SidebarPanel) {
        self.panel = panel;
        self.dirty = true;
    }
}
