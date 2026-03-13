use crate::gpu::atlas::{GlyphAtlas, GlyphKey};
use crate::gpu::context::GpuCell;

use super::discovery::ClaudeStatus;
use super::state::{AccountUsage, SidebarHitEntry, SidebarPanel, SidebarState, TabSessionEntry};

// Sidebar colors — refined dark palette with good contrast
const SIDEBAR_BG: [f32; 4] = [0.020, 0.180, 0.220, 1.0]; // deep teal-black
const CARD_BG: [f32; 4] = [0.030, 0.200, 0.245, 1.0]; // slightly lifted from sidebar
const CARD_ACTIVE_BG: [f32; 4] = [0.045, 0.230, 0.280, 1.0]; // active card lift
const HEADER_FG: [f32; 4] = [0.165, 0.631, 0.596, 1.0]; // cyan accent
const LABEL_FG: [f32; 4] = [0.475, 0.545, 0.560, 1.0]; // muted label text
const VALUE_FG: [f32; 4] = [0.870, 0.855, 0.790, 1.0]; // warm white
const DIM_FG: [f32; 4] = [0.310, 0.395, 0.420, 1.0]; // subdued info
const GREEN_FG: [f32; 4] = [0.400, 0.680, 0.350, 1.0]; // softer green
const YELLOW_FG: [f32; 4] = [0.780, 0.600, 0.150, 1.0]; // warm amber
const RED_FG: [f32; 4] = [0.850, 0.280, 0.250, 1.0]; // clear red
const BAR_EMPTY: [f32; 4] = [0.050, 0.220, 0.260, 1.0]; // subtle empty track
const SEPARATOR_FG: [f32; 4] = [0.060, 0.250, 0.300, 1.0]; // very subtle divider
const HOVER_BG: [f32; 4] = [0.055, 0.250, 0.305, 1.0]; // gentle hover lift
const ACCENT_LEFT: [f32; 4] = [0.165, 0.631, 0.596, 1.0]; // cyan for active indicator

// Layout constants — comfortable spacing
const PAD_LEFT: u16 = 2; // left margin inside sidebar
const PAD_RIGHT: u16 = 1; // right margin
const CARD_PAD_LEFT: u16 = 3; // indent inside cards
const CARD_PAD_INNER: u16 = 4; // indent for sub-content inside cards

/// Usable content width given the sidebar column count.
fn content_width(cols: u16) -> u16 {
    cols.saturating_sub(PAD_LEFT + PAD_RIGHT)
}

/// Max column for text (exclusive) — prevents right-edge clipping.
fn max_col(cols: u16) -> u16 {
    cols.saturating_sub(PAD_RIGHT)
}

/// Render the sidebar, returning (cells, content_height).
/// `scroll_offset` shifts the viewport down by that many rows.
pub fn render_sidebar(
    state: &SidebarState,
    cols: u16,
    rows: u16,
    active_tab: usize,
    hovered_tab: Option<usize>,
    scroll_offset: u16,
    atlas: &mut GlyphAtlas,
    hit_map_out: &mut Vec<SidebarHitEntry>,
) -> (Vec<GpuCell>, u16) {
    // Render into a virtual buffer tall enough for all content.
    // Use 4x visible rows as a generous upper bound.
    let virtual_rows = (rows as usize * 4).max(200);
    let virtual_count = cols as usize * virtual_rows;
    let mut virtual_cells = vec![GpuCell::default(); virtual_count];

    for cell in &mut virtual_cells {
        cell.bg_color = SIDEBAR_BG;
    }

    hit_map_out.clear();

    let content_height = match state.panel {
        SidebarPanel::Usage => {
            render_usage_panel(&mut virtual_cells, cols, virtual_rows as u16, &state.accounts, atlas)
        }
        SidebarPanel::Sessions => {
            render_sessions_panel(
                &mut virtual_cells,
                cols,
                virtual_rows as u16,
                &state.tab_entries,
                active_tab,
                hovered_tab,
                atlas,
                hit_map_out,
            )
        }
        SidebarPanel::Output => {
            render_output_panel(&mut virtual_cells, cols, virtual_rows as u16, atlas)
        }
    };

    // Copy visible window from virtual buffer into output, applying scroll offset.
    let out_count = cols as usize * rows as usize;
    let mut cells = vec![GpuCell::default(); out_count];
    for cell in &mut cells {
        cell.bg_color = SIDEBAR_BG;
    }

    let offset = scroll_offset as usize;
    for row in 0..rows as usize {
        let src_row = row + offset;
        if src_row >= virtual_rows {
            break;
        }
        let src_start = src_row * cols as usize;
        let dst_start = row * cols as usize;
        for col in 0..cols as usize {
            if let (Some(src), Some(dst)) = (
                virtual_cells.get(src_start + col),
                cells.get_mut(dst_start + col),
            ) {
                *dst = *src;
            }
        }
    }

    // Adjust hit_map entries by scroll offset
    for entry in hit_map_out.iter_mut() {
        entry.start_row = entry.start_row.saturating_sub(scroll_offset);
        entry.end_row = entry.end_row.saturating_sub(scroll_offset);
    }
    // Remove entries that scrolled off screen
    hit_map_out.retain(|e| e.end_row > 0 && e.start_row < rows);

    (cells, content_height)
}

fn render_usage_panel(
    cells: &mut [GpuCell],
    cols: u16,
    _rows: u16,
    accounts: &[AccountUsage],
    atlas: &mut GlyphAtlas,
) -> u16 {
    let mc = max_col(cols);
    let mut row: u16 = 1;

    // Section header
    let header = format!("USAGE ({})", accounts.len());
    write_str(cells, cols, row, PAD_LEFT, mc, &header, HEADER_FG, SIDEBAR_BG, atlas);
    row += 2;
    write_separator(cells, cols, row, PAD_LEFT, mc, atlas);
    row += 2;

    if accounts.is_empty() {
        write_str(cells, cols, row, PAD_LEFT, mc, "Loading...", DIM_FG, SIDEBAR_BG, atlas);
        return row + 1;
    }

    for (i, account) in accounts.iter().enumerate() {
        let cw = content_width(cols) as usize;
        let short_name: String = account
            .account_name
            .split('@')
            .next()
            .unwrap_or(&account.account_name)
            .chars()
            .take(cw)
            .collect();

        // Card background area starts here
        let card_start = row;

        // Account name with padding row above
        write_str(cells, cols, row, CARD_PAD_LEFT, mc, &short_name, VALUE_FG, CARD_BG, atlas);
        row += 2;

        let data = &account.data;

        // 5-hour window: label left, percentage right
        let label = "5h window";
        let pct_str = format!("{}%", data.utilization);
        let pct_fg = utilization_color(data.utilization);
        write_str(cells, cols, row, CARD_PAD_LEFT, mc, label, LABEL_FG, CARD_BG, atlas);
        let pct_col = mc.saturating_sub(pct_str.len() as u16);
        write_str(cells, cols, row, pct_col, mc, &pct_str, pct_fg, CARD_BG, atlas);
        row += 1;

        // Smooth progress bar
        let bar_width = mc.saturating_sub(CARD_PAD_LEFT);
        render_progress_bar(cells, cols, row, CARD_PAD_LEFT, bar_width, data.utilization, atlas);
        row += 1;

        // Reset time
        if let Some(ref resets_at) = data.resets_at {
            let now = chrono::Utc::now();
            let remaining = (*resets_at - now).num_minutes().max(0);
            let hours = remaining / 60;
            let mins = remaining % 60;
            let reset_str = format!("\u{21BB} {hours}h {mins}m", ); // ↻ reset icon
            write_str(cells, cols, row, CARD_PAD_LEFT, mc, &reset_str, DIM_FG, CARD_BG, atlas);
            row += 1;
        }

        row += 1; // spacer before weekly

        // Weekly window
        if let Some(weekly_util) = data.weekly_utilization {
            let wlabel = "7d window";
            let wpct_str = format!("{weekly_util}%");
            let wpct_fg = utilization_color(weekly_util);
            write_str(cells, cols, row, CARD_PAD_LEFT, mc, wlabel, LABEL_FG, CARD_BG, atlas);
            let wpct_col = mc.saturating_sub(wpct_str.len() as u16);
            write_str(cells, cols, row, wpct_col, mc, &wpct_str, wpct_fg, CARD_BG, atlas);
            row += 1;

            let bar_width = mc.saturating_sub(CARD_PAD_LEFT);
            render_progress_bar(cells, cols, row, CARD_PAD_LEFT, bar_width, weekly_util, atlas);
            row += 1;

            if let Some(ref wresets) = data.weekly_resets_at {
                let now = chrono::Utc::now();
                let remaining = (*wresets - now).num_minutes().max(0);
                let days = remaining / (60 * 24);
                let hours = (remaining % (60 * 24)) / 60;
                let wreset_str = format!("\u{21BB} {days}d {hours}h");
                write_str(cells, cols, row, CARD_PAD_LEFT, mc, &wreset_str, DIM_FG, CARD_BG, atlas);
                row += 1;
            }
        }

        // Fill card background for the entire account block
        fill_card_bg(cells, cols, card_start, row, PAD_LEFT, mc, CARD_BG);

        row += 1; // breathing room between account cards

        // Subtle separator between accounts (not after last)
        if i + 1 < accounts.len() {
            write_separator(cells, cols, row, CARD_PAD_LEFT, mc, atlas);
            row += 1;
        }
    }
    row
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
) -> u16 {
    let mc = max_col(cols);
    let mut row: u16 = 1;

    // Section header
    let header = format!("SESSIONS ({})", entries.len());
    write_str(cells, cols, row, PAD_LEFT, mc, &header, HEADER_FG, SIDEBAR_BG, atlas);
    row += 2;
    write_separator(cells, cols, row, PAD_LEFT, mc, atlas);
    row += 2;

    if entries.is_empty() {
        write_str(cells, cols, row, PAD_LEFT, mc, "No sessions", DIM_FG, SIDEBAR_BG, atlas);
        return row + 1;
    }

    for entry in entries {
        let card_start_row = row;
        let is_active = entry.tab_index == active_tab;
        let is_hovered = hovered_tab == Some(entry.tab_index);
        let bg = if is_active {
            CARD_ACTIVE_BG
        } else if is_hovered {
            HOVER_BG
        } else {
            CARD_BG
        };

        // Content column starts after the left accent bar
        let text_col = CARD_PAD_LEFT + 1; // leave col CARD_PAD_LEFT-1..CARD_PAD_LEFT for accent

        match entry.session {
            Some(ref session) => {
                let (status_icon, status_fg) = match session.status {
                    ClaudeStatus::Working => ("\u{25CF}", YELLOW_FG), // ● working
                    ClaudeStatus::Idle => ("\u{25CF}", GREEN_FG),     // ● idle
                };

                // Line 1: status dot + display title
                write_str(cells, cols, row, text_col, mc, status_icon, status_fg, bg, atlas);
                let title_col = text_col + 2;
                let title: String = entry.display_title.chars().take(mc.saturating_sub(title_col) as usize).collect();
                write_str(cells, cols, row, title_col, mc, &title, VALUE_FG, bg, atlas);
                row += 1;

                // Line 2: project folder
                if !session.project.is_empty() {
                    let proj: String = session.project.chars().take(mc.saturating_sub(CARD_PAD_INNER + 1) as usize).collect();
                    write_str(cells, cols, row, CARD_PAD_INNER + 1, mc, &proj, LABEL_FG, bg, atlas);
                    row += 1;
                }

                // Line 3: session ID (abbreviated)
                let short_id: String = session.session_id.chars().take(8).collect();
                write_str(cells, cols, row, CARD_PAD_INNER + 1, mc, &short_id, DIM_FG, bg, atlas);
                row += 1;
            }
            None => {
                // No session — dim empty state
                write_str(cells, cols, row, text_col, mc, "\u{25CB}", DIM_FG, bg, atlas);
                let title_col = text_col + 2;
                let title: String = entry.display_title.chars().take(mc.saturating_sub(title_col) as usize).collect();
                write_str(cells, cols, row, title_col, mc, &title, DIM_FG, bg, atlas);
                row += 1;
            }
        }

        // Fill card background
        fill_card_bg(cells, cols, card_start_row, row, PAD_LEFT, mc, bg);

        // Left accent bar for active tab (thin vertical stripe)
        if is_active {
            paint_left_accent(cells, cols, card_start_row, row, PAD_LEFT, ACCENT_LEFT, atlas);
        }

        // Record hit area for click/hover detection
        hit_map.push(SidebarHitEntry {
            start_row: card_start_row,
            end_row: row,
            tab_index: entry.tab_index,
        });

        row += 1; // breathing room between cards
    }
    row
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
) -> u16 {
    let mc = max_col(cols);
    let mut row: u16 = 1;

    write_str(cells, cols, row, PAD_LEFT, mc, "SHORTCUTS", HEADER_FG, SIDEBAR_BG, atlas);
    row += 2;
    write_separator(cells, cols, row, PAD_LEFT, mc, atlas);
    row += 2;

    // Key bindings laid out as key + description
    let shortcuts = [
        ("\u{2318}B", "Toggle sidebar"),
        ("\u{2318}\u{21E7}B", "Cycle panel"),
        ("\u{2318}T", "New tab"),
        ("\u{2318}W", "Close tab"),
        ("\u{2318}\u{21E7}R", "Rename tab"),
    ];

    for (key, desc) in &shortcuts {
        // Key in accent color, description in dim
        write_str(cells, cols, row, CARD_PAD_LEFT, mc, key, HEADER_FG, SIDEBAR_BG, atlas);
        let desc_col = CARD_PAD_LEFT + 5; // consistent description alignment
        write_str(cells, cols, row, desc_col, mc, desc, LABEL_FG, SIDEBAR_BG, atlas);
        row += 2; // double spacing for readability
    }
    row
}

/// Paint a thin vertical accent on the left edge of a card row range.
fn paint_left_accent(
    cells: &mut [GpuCell],
    cols: u16,
    start_row: u16,
    end_row: u16,
    col: u16,
    color: [f32; 4],
    atlas: &mut GlyphAtlas,
) {
    let ch = '\u{2503}'; // ┃ heavy vertical box-drawing
    let key = GlyphKey { ch, bold: false, italic: false };
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

    for r in start_row..end_row {
        let offset = r as usize * cols as usize + col as usize;
        if let Some(cell) = cells.get_mut(offset) {
            *cell = GpuCell {
                glyph_index: ch as u32,
                fg_color: color,
                bg_color: cell.bg_color, // preserve card bg
                flags: 0,
                atlas_uv_x: uv_x,
                atlas_uv_y: uv_y,
                atlas_uv_w: uv_w,
                atlas_uv_h: uv_h,
            };
        }
    }
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
    // Thin horizontal rule using light box-drawing character
    let width = end_col.saturating_sub(start_col) as usize;
    let sep: String = (0..width).map(|_| '\u{2500}').collect(); // ─ light horizontal
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

    // Use half-height blocks for a sleeker bar: ▄ (lower half) for filled, ▁ (lower 1/8) for empty
    for i in 0..width {
        let col = start_col + i;
        if col >= cols {
            break;
        }
        let (ch, fg, bg) = if i < filled {
            ('\u{2584}', fill_fg, CARD_BG) // ▄ lower half block — sleek fill
        } else {
            ('\u{2581}', BAR_EMPTY, CARD_BG) // ▁ lower 1/8 block — subtle track
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

