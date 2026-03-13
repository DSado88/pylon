use crate::gpu::atlas::{GlyphAtlas, GlyphKey};
use crate::gpu::context::GpuCell;

use super::discovery::ClaudeStatus;
use super::state::{AccountUsage, SidebarHitEntry, SidebarPanel, SidebarState, TabSessionEntry};

// Sidebar colors (Solarized Dark palette)
const SIDEBAR_BG: [f32; 4] = [0.027, 0.212, 0.259, 1.0]; // base02
const CARD_BG: [f32; 4] = [0.012, 0.190, 0.235, 1.0]; // slightly darker than sidebar
const HEADER_FG: [f32; 4] = [0.165, 0.631, 0.596, 1.0]; // cyan
const LABEL_FG: [f32; 4] = [0.514, 0.580, 0.588, 1.0]; // base0
const VALUE_FG: [f32; 4] = [0.933, 0.910, 0.835, 1.0]; // base2
const DIM_FG: [f32; 4] = [0.345, 0.431, 0.459, 1.0]; // base01
const GREEN_FG: [f32; 4] = [0.522, 0.600, 0.000, 1.0]; // green
const YELLOW_FG: [f32; 4] = [0.710, 0.537, 0.000, 1.0]; // yellow
const RED_FG: [f32; 4] = [0.863, 0.196, 0.184, 1.0]; // red
const BAR_EMPTY: [f32; 4] = [0.000, 0.169, 0.212, 1.0]; // base03
const SEPARATOR_FG: [f32; 4] = [0.075, 0.280, 0.329, 1.0]; // subtle line
const HOVER_BG: [f32; 4] = [0.050, 0.260, 0.310, 1.0]; // lighter on hover

// Layout constants — tight but comfortable
const PAD_LEFT: u16 = 1; // left margin inside sidebar
const PAD_RIGHT: u16 = 1; // right margin (content won't render past cols - PAD_RIGHT)
const CARD_PAD_LEFT: u16 = 2; // extra indent inside cards
const CARD_PAD_INNER: u16 = 3; // indent for sub-content inside cards

/// Usable content width given the sidebar column count.
fn content_width(cols: u16) -> u16 {
    cols.saturating_sub(PAD_LEFT + PAD_RIGHT)
}

/// Max column for text (exclusive) — prevents right-edge clipping.
fn max_col(cols: u16) -> u16 {
    cols.saturating_sub(PAD_RIGHT)
}

pub fn render_sidebar(
    state: &SidebarState,
    cols: u16,
    rows: u16,
    active_tab: usize,
    atlas: &mut GlyphAtlas,
    hit_map_out: &mut Vec<SidebarHitEntry>,
) -> Vec<GpuCell> {
    let count = cols as usize * rows as usize;
    let mut cells = vec![GpuCell::default(); count];

    // Fill background
    for cell in &mut cells {
        cell.bg_color = SIDEBAR_BG;
    }

    hit_map_out.clear();

    match state.panel {
        SidebarPanel::Usage => render_usage_panel(&mut cells, cols, rows, &state.accounts, atlas),
        SidebarPanel::Sessions => {
            render_sessions_panel(
                &mut cells,
                cols,
                rows,
                &state.tab_entries,
                active_tab,
                state.hovered_tab,
                atlas,
                hit_map_out,
            );
        }
        SidebarPanel::Output => {
            render_output_panel(&mut cells, cols, rows, atlas);
        }
    }

    cells
}

fn render_usage_panel(
    cells: &mut [GpuCell],
    cols: u16,
    _rows: u16,
    accounts: &[AccountUsage],
    atlas: &mut GlyphAtlas,
) {
    let mc = max_col(cols);
    let mut row: u16 = 1; // top padding

    // Header
    let header = format!("USAGE ({})", accounts.len());
    write_str(cells, cols, row, PAD_LEFT, mc, &header, HEADER_FG, SIDEBAR_BG, atlas);
    row += 2;
    write_separator(cells, cols, row, PAD_LEFT, mc, atlas);
    row += 2;

    if accounts.is_empty() {
        write_str(cells, cols, row, PAD_LEFT, mc, "Loading...", DIM_FG, SIDEBAR_BG, atlas);
        return;
    }

    for account in accounts {
        let cw = content_width(cols) as usize;
        let short_name: String = account
            .account_name
            .split('@')
            .next()
            .unwrap_or(&account.account_name)
            .chars()
            .take(cw)
            .collect();
        write_str(cells, cols, row, PAD_LEFT, mc, &short_name, VALUE_FG, SIDEBAR_BG, atlas);
        row += 2;

        let data = &account.data;

        // 5-hour window
        let label = "5h";
        let pct_str = format!("{}%", data.utilization);
        let pct_fg = utilization_color(data.utilization);
        write_str(cells, cols, row, CARD_PAD_LEFT, mc, label, LABEL_FG, SIDEBAR_BG, atlas);
        let pct_col = mc.saturating_sub(pct_str.len() as u16);
        write_str(cells, cols, row, pct_col, mc, &pct_str, pct_fg, SIDEBAR_BG, atlas);
        row += 1;

        // Progress bar
        let bar_width = mc.saturating_sub(CARD_PAD_LEFT + 1);
        render_progress_bar(cells, cols, row, CARD_PAD_LEFT, bar_width, data.utilization, atlas);
        row += 2;

        // Reset time
        if let Some(ref resets_at) = data.resets_at {
            let now = chrono::Utc::now();
            let remaining = (*resets_at - now).num_minutes().max(0);
            let hours = remaining / 60;
            let mins = remaining % 60;
            let reset_str = format!("resets {hours}h {mins}m");
            write_str(cells, cols, row, CARD_PAD_LEFT, mc, &reset_str, DIM_FG, SIDEBAR_BG, atlas);
            row += 2;
        }

        // Weekly window
        if let Some(weekly_util) = data.weekly_utilization {
            let wlabel = "7d";
            let wpct_str = format!("{weekly_util}%");
            let wpct_fg = utilization_color(weekly_util);
            write_str(cells, cols, row, CARD_PAD_LEFT, mc, wlabel, LABEL_FG, SIDEBAR_BG, atlas);
            let wpct_col = mc.saturating_sub(wpct_str.len() as u16);
            write_str(cells, cols, row, wpct_col, mc, &wpct_str, wpct_fg, SIDEBAR_BG, atlas);
            row += 1;

            let bar_width = mc.saturating_sub(CARD_PAD_LEFT + 1);
            render_progress_bar(cells, cols, row, CARD_PAD_LEFT, bar_width, weekly_util, atlas);
            row += 2;

            if let Some(ref wresets) = data.weekly_resets_at {
                let now = chrono::Utc::now();
                let remaining = (*wresets - now).num_minutes().max(0);
                let days = remaining / (60 * 24);
                let hours = (remaining % (60 * 24)) / 60;
                let wreset_str = format!("resets {days}d {hours}h");
                write_str(cells, cols, row, CARD_PAD_LEFT, mc, &wreset_str, DIM_FG, SIDEBAR_BG, atlas);
                row += 2;
            }
        }

        row += 1; // extra spacing between accounts
    }
}

#[allow(clippy::too_many_arguments)]
fn render_sessions_panel(
    cells: &mut [GpuCell],
    cols: u16,
    _rows: u16,
    entries: &[TabSessionEntry],
    active_tab: usize,
    hovered_tab: Option<usize>,
    atlas: &mut GlyphAtlas,
    hit_map: &mut Vec<SidebarHitEntry>,
) {
    let mc = max_col(cols);
    let mut row: u16 = 1; // top padding

    // Header
    let header = format!("SESSIONS ({})", entries.len());
    write_str(cells, cols, row, PAD_LEFT, mc, &header, HEADER_FG, SIDEBAR_BG, atlas);
    row += 2;
    write_separator(cells, cols, row, PAD_LEFT, mc, atlas);
    row += 2;

    if entries.is_empty() {
        write_str(cells, cols, row, PAD_LEFT, mc, "No tabs", DIM_FG, SIDEBAR_BG, atlas);
        return;
    }

    let _card_inner = mc.saturating_sub(CARD_PAD_LEFT);

    for entry in entries {
        let card_start_row = row;
        let is_active = entry.tab_index == active_tab;
        let is_hovered = hovered_tab == Some(entry.tab_index);
        let bg = if is_active || is_hovered { HOVER_BG } else { CARD_BG };

        match entry.session {
            Some(ref session) => {
                // Status dot + tab number + title on same line
                let (status_icon, status_fg) = match session.status {
                    ClaudeStatus::Working => ("\u{25CF}", YELLOW_FG), // ● filled circle
                    ClaudeStatus::Idle => ("\u{25CF}", GREEN_FG),     // ● filled circle
                };
                // Line 1: status dot + display title
                write_str(cells, cols, row, CARD_PAD_LEFT, mc, status_icon, status_fg, bg, atlas);
                let title_col = CARD_PAD_LEFT + 1;
                let title: String = entry.display_title.chars().take(mc.saturating_sub(title_col) as usize).collect();
                write_str(cells, cols, row, title_col, mc, &title, VALUE_FG, bg, atlas);
                row += 1;

                // Line 2: project folder
                if !session.project.is_empty() {
                    let proj: String = session.project.chars().take(mc.saturating_sub(CARD_PAD_INNER) as usize).collect();
                    write_str(cells, cols, row, CARD_PAD_INNER, mc, &proj, LABEL_FG, bg, atlas);
                    row += 1;
                }

                // Line 3: first 8 chars of session_id
                let short_id: String = session.session_id.chars().take(8).collect();
                write_str(cells, cols, row, CARD_PAD_INNER, mc, &short_id, DIM_FG, bg, atlas);
                row += 1;
            }
            None => {
                // No session — dim card with dot + title
                write_str(cells, cols, row, CARD_PAD_LEFT, mc, "\u{25CB}", DIM_FG, bg, atlas);
                let title_col = CARD_PAD_LEFT + 1;
                let title: String = entry.display_title.chars().take(mc.saturating_sub(title_col) as usize).collect();
                write_str(cells, cols, row, title_col, mc, &title, DIM_FG, bg, atlas);
                row += 1;
            }
        }

        // Fill card background for all rows in this entry
        fill_card_bg(cells, cols, card_start_row, row, CARD_PAD_LEFT.saturating_sub(1), mc, bg);

        // Record hit area for click/hover detection
        hit_map.push(SidebarHitEntry {
            start_row: card_start_row,
            end_row: row,
            tab_index: entry.tab_index,
        });

        row += 1; // breathing room between cards
    }
}

/// Fill card background color for a range of rows.
fn fill_card_bg(cells: &mut [GpuCell], cols: u16, start_row: u16, end_row: u16, start_col: u16, end_col: u16, bg: [f32; 4]) {
    for r in start_row..end_row {
        for c in start_col..end_col {
            let offset = r as usize * cols as usize + c as usize;
            if let Some(cell) = cells.get_mut(offset) {
                if cell.glyph_index == 0 {
                    cell.bg_color = bg;
                }
            }
        }
    }
}

fn render_output_panel(
    cells: &mut [GpuCell],
    cols: u16,
    _rows: u16,
    atlas: &mut GlyphAtlas,
) {
    let mc = max_col(cols);
    let mut row: u16 = 1;

    write_str(cells, cols, row, PAD_LEFT, mc, "OUTPUT", HEADER_FG, SIDEBAR_BG, atlas);
    row += 2;
    write_separator(cells, cols, row, PAD_LEFT, mc, atlas);
    row += 2;

    write_str(cells, cols, row, CARD_PAD_LEFT, mc, "Cmd+B  Toggle sidebar", DIM_FG, SIDEBAR_BG, atlas);
    row += 1;
    write_str(cells, cols, row, CARD_PAD_LEFT, mc, "Cmd+Shift+B  Cycle panel", DIM_FG, SIDEBAR_BG, atlas);
    row += 1;
    write_str(cells, cols, row, CARD_PAD_LEFT, mc, "Cmd+T  New tab", DIM_FG, SIDEBAR_BG, atlas);
    row += 1;
    write_str(cells, cols, row, CARD_PAD_LEFT, mc, "Cmd+W  Close tab", DIM_FG, SIDEBAR_BG, atlas);
    row += 1;
    write_str(cells, cols, row, CARD_PAD_LEFT, mc, "Cmd+Shift+R  Rename tab", DIM_FG, SIDEBAR_BG, atlas);
}

#[allow(clippy::too_many_arguments)]
fn write_str(
    cells: &mut [GpuCell],
    cols: u16,
    row: u16,
    col: u16,
    max_col: u16,
    text: &str,
    fg: [f32; 4],
    bg: [f32; 4],
    atlas: &mut GlyphAtlas,
) {
    let mut c = col;
    for ch in text.chars() {
        if c >= max_col {
            break;
        }
        let offset = row as usize * cols as usize + c as usize;

        let key = GlyphKey {
            ch,
            bold: false,
            italic: false,
        };
        let (uv_x, uv_y, uv_w, uv_h) = match atlas.get_or_insert(key) {
            Ok(entry) => {
                let aw = atlas.atlas_width();
                let ah = atlas.atlas_height();
                (
                    entry.x as f32 / aw,
                    entry.y as f32 / ah,
                    entry.width as f32 / aw,
                    entry.height as f32 / ah,
                )
            }
            Err(_) => (0.0, 0.0, 0.0, 0.0),
        };

        if let Some(cell) = cells.get_mut(offset) {
            *cell = GpuCell {
                glyph_index: ch as u32,
                fg_color: fg,
                bg_color: bg,
                flags: 0,
                atlas_uv_x: uv_x,
                atlas_uv_y: uv_y,
                atlas_uv_w: uv_w,
                atlas_uv_h: uv_h,
            };
        }
        c += 1;
    }
}

fn write_separator(cells: &mut [GpuCell], cols: u16, row: u16, start_col: u16, end_col: u16, atlas: &mut GlyphAtlas) {
    // Thin line using Unicode horizontal bar
    let width = end_col.saturating_sub(start_col) as usize;
    let sep: String = (0..width).map(|_| '\u{2500}').collect(); // ─ box drawing
    write_str(cells, cols, row, start_col, end_col, &sep, SEPARATOR_FG, SIDEBAR_BG, atlas);
}

fn render_progress_bar(
    cells: &mut [GpuCell],
    cols: u16,
    row: u16,
    start_col: u16,
    width: u16,
    pct: u32,
    atlas: &mut GlyphAtlas,
) {
    let filled = ((pct as f32 / 100.0) * width as f32).round() as u16;
    let fill_fg = utilization_color(pct);

    for i in 0..width {
        let col = start_col + i;
        if col >= cols {
            break;
        }
        let (ch, fg, bg) = if i < filled {
            ('\u{2588}', fill_fg, SIDEBAR_BG) // █ full block
        } else {
            ('\u{2591}', BAR_EMPTY, SIDEBAR_BG) // ░ light shade
        };
        let offset = row as usize * cols as usize + col as usize;

        let key = GlyphKey {
            ch,
            bold: false,
            italic: false,
        };
        let (uv_x, uv_y, uv_w, uv_h) = match atlas.get_or_insert(key) {
            Ok(entry) => {
                let aw = atlas.atlas_width();
                let ah = atlas.atlas_height();
                (
                    entry.x as f32 / aw,
                    entry.y as f32 / ah,
                    entry.width as f32 / aw,
                    entry.height as f32 / ah,
                )
            }
            Err(_) => (0.0, 0.0, 0.0, 0.0),
        };

        if let Some(cell) = cells.get_mut(offset) {
            *cell = GpuCell {
                glyph_index: ch as u32,
                fg_color: fg,
                bg_color: bg,
                flags: 0,
                atlas_uv_x: uv_x,
                atlas_uv_y: uv_y,
                atlas_uv_w: uv_w,
                atlas_uv_h: uv_h,
            };
        }
    }
}

fn utilization_color(pct: u32) -> [f32; 4] {
    if pct >= 80 {
        RED_FG
    } else if pct >= 50 {
        YELLOW_FG
    } else {
        GREEN_FG
    }
}

