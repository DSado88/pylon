use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{mpsc, watch};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::platform::macos::{WindowAttributesExtMacOS, WindowExtMacOS};
use winit::window::{CursorIcon, WindowAttributes, WindowId};

use crate::event_policy::{self, FrameResult};

use crate::config::CockpitConfig;
use crate::error::{CockpitError, Result};
use crate::gpu::atlas::{GlyphAtlas, GlyphKey};
use crate::gpu::context::GpuCell;
use crate::gpu::renderer::TerminalRenderer;
use crate::gpu::window::CockpitWindow;
use crate::grid::cell::{CellFlags, Color, NamedColor};
use crate::grid::storage::{Grid, SharedGrid};
use crate::primitives::DirtyRows;
use crate::pty::reader::PtyReader;
use crate::pty::spawn::{PtyHandle, PtySize};
use crate::sidebar::discovery;
use crate::sidebar::layout;
use crate::sidebar::state::{SidebarState, TabSessionEntry};
use crate::sidebar::usage;
use crate::vt::parser::VtParser;
use crate::vt::state::TerminalState;

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;
const TABBING_ID: &str = "cockpit-terminal";

/// Per-window terminal state. Each native macOS tab is a window with its own
/// renderer, PTY, grid, and VT parser.
struct TerminalWindow {
    cockpit_window: CockpitWindow,
    renderer: TerminalRenderer,
    grid: SharedGrid,
    dirty_rows: Arc<DirtyRows>,
    pty: PtyHandle,
    pty_reader: PtyReader,
    vt_parser: VtParser,
    vt_state: TerminalState,
    grid_cols: u16,
    grid_rows: u16,
    sidebar_cols: u16,
    /// Sidebar version last rendered by this window (for dirty detection).
    sidebar_rendered_version: u64,
    /// Claude session detected as a child of this tab's shell.
    claude_session: Option<discovery::ClaudeSession>,
    /// User-set custom title. Overrides all auto-detected titles.
    custom_title: Option<String>,
    /// When true, keyboard input goes to title_edit_buffer instead of PTY.
    renaming_tab: bool,
    /// Buffer for in-progress tab rename.
    title_edit_buffer: String,
    /// Last title set on the native window, to skip redundant set_title calls.
    last_set_title: String,
    /// Scroll offset: 0 = live (bottom), >0 = scrolled up N lines into history.
    scroll_offset: usize,
}

impl TerminalWindow {
    /// Poll PTY for output, feed through VT parser into grid. Returns true if data was read.
    fn poll_pty(&mut self) -> bool {
        let data = match self
            .pty_reader
            .poll_read(self.pty.raw_fd(), Duration::from_millis(0))
        {
            Ok(d) if !d.is_empty() => d.to_vec(),
            _ => return false,
        };

        if let Ok(mut grid) = self.grid.write() {
            self.vt_parser
                .process(&data, &mut grid, &mut self.vt_state, &self.dirty_rows);
        }
        // Snap to bottom on new output
        if self.scroll_offset > 0 {
            self.scroll_offset = 0;
            self.dirty_rows.mark_all();
        }
        true
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        if let Ok(mut grid) = self.grid.write() {
            grid.resize(rows as usize, cols as usize);
        }
        self.vt_state.scroll_region = (0, rows);
        self.vt_state.clamp_cursor(rows, cols);
        let _ = self.pty.resize(PtySize::new(cols, rows));
        self.dirty_rows.mark_all();
        self.grid_cols = cols;
        self.grid_rows = rows;
    }

    fn write_pty(&self, data: &[u8]) {
        let _ = self.pty.write(data);
    }
}

pub struct App {
    windows: Vec<(WindowId, TerminalWindow)>,
    sidebar: SidebarState,
    /// Incremented whenever sidebar data changes. Each window compares against
    /// its own `sidebar_rendered_version` to know when to re-render.
    sidebar_version: u64,
    config: CockpitConfig,
    modifiers: ModifiersState,
    // Tokio runtime handle for spawning async tasks
    rt_handle: tokio::runtime::Handle,
    // Channels from async pollers
    usage_rx: Option<mpsc::UnboundedReceiver<usage::UsageUpdate>>,
    sessions_req_tx: Option<watch::Sender<Vec<discovery::TabScanRequest>>>,
    sessions_rx: Option<mpsc::UnboundedReceiver<Vec<discovery::TabScanResult>>>,
    pollers_started: bool,
    // Sidebar drag-to-resize state and cursor tracking
    cursor_x: f64,
    cursor_y: f64,
    dragging_sidebar: bool,
    /// Index of the currently focused/active tab (for sidebar highlight).
    active_tab: usize,
    // Double-click detection for sidebar cards
    last_sidebar_click_time: Option<Instant>,
    last_sidebar_click_tab: Option<usize>,
    // Cursor blink state
    cursor_blink_visible: bool,
    cursor_blink_last_toggle: Instant,
    // Text selection state
    selection: Option<Selection>,
    selecting: bool,
}

/// A text selection range in grid coordinates.
#[derive(Debug, Clone, Copy)]
struct Selection {
    start_row: usize,
    start_col: usize,
    end_row: usize,
    end_col: usize,
}

/// Convert a physical pixel position to grid (row, col), accounting for
/// scale factor and padding.
fn pixel_to_grid(
    x: f64,
    y: f64,
    cell_w: f32,
    cell_h: f32,
    pad: f32,
    scale: f64,
    cols: u16,
    rows: u16,
) -> (usize, usize) {
    let lx = (x / scale) as f32 - pad;
    let ly = (y / scale) as f32 - pad;
    let col = (lx / cell_w).max(0.0) as usize;
    let row = (ly / cell_h).max(0.0) as usize;
    (
        row.min(rows.saturating_sub(1) as usize),
        col.min(cols.saturating_sub(1) as usize),
    )
}

impl Selection {
    /// Return (top_row, top_col, bottom_row, bottom_col) in normalized order.
    fn ordered(&self) -> (usize, usize, usize, usize) {
        if self.start_row < self.end_row
            || (self.start_row == self.end_row && self.start_col <= self.end_col)
        {
            (self.start_row, self.start_col, self.end_row, self.end_col)
        } else {
            (self.end_row, self.end_col, self.start_row, self.start_col)
        }
    }

    /// Check if a cell at (row, col) is within this selection.
    fn contains(&self, row: usize, col: usize) -> bool {
        let (r0, c0, r1, c1) = self.ordered();
        if row < r0 || row > r1 {
            return false;
        }
        if r0 == r1 {
            col >= c0 && col <= c1
        } else if row == r0 {
            col >= c0
        } else if row == r1 {
            col <= c1
        } else {
            true
        }
    }
}

impl App {
    pub fn new(config: CockpitConfig, rt_handle: tokio::runtime::Handle) -> Self {
        let sidebar = SidebarState::new(&config);

        Self {
            windows: Vec::new(),
            sidebar,
            sidebar_version: 1, // start at 1 so initial render triggers
            config,
            modifiers: ModifiersState::empty(),
            rt_handle,
            usage_rx: None,
            sessions_req_tx: None,
            sessions_rx: None,
            pollers_started: false,
            cursor_x: 0.0,
            cursor_y: 0.0,
            dragging_sidebar: false,
            active_tab: 0,
            last_sidebar_click_time: None,
            last_sidebar_click_tab: None,
            cursor_blink_visible: true,
            cursor_blink_last_toggle: Instant::now(),
            selection: None,
            selecting: false,
        }
    }

    /// Create a new terminal window with its own renderer, PTY, and grid.
    fn create_terminal_window(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Result<(WindowId, TerminalWindow)> {
        let attrs = WindowAttributes::default()
            .with_title("Claude Cockpit")
            .with_inner_size(winit::dpi::LogicalSize::new(1440.0, 900.0))
            .with_tabbing_identifier(TABBING_ID);

        let window = event_loop
            .create_window(attrs)
            .map_err(|e| CockpitError::Render(format!("create window: {e}")))?;

        let renderer = TerminalRenderer::new(
            DEFAULT_COLS as u32,
            DEFAULT_ROWS as u32,
            &self.config.font_family,
            self.config.font_size,
        )?;

        let cockpit_window = CockpitWindow::from_window(window, &renderer.ctx.device)?;

        // Set initial drawable size
        let phys_size = cockpit_window.window.inner_size();
        let scale = cockpit_window.window.scale_factor() as f32;
        cockpit_window
            .metal_layer
            .setDrawableSize(objc2_foundation::NSSize::new(
                phys_size.width as f64,
                phys_size.height as f64,
            ));

        // Compute grid dimensions from logical (point) size
        let logical_w = phys_size.width as f32 / scale;
        let logical_h = phys_size.height as f32 / scale;
        let cell_w = renderer.atlas.cell_width;
        let cell_h = renderer.atlas.cell_height;

        let sidebar_logical = if self.sidebar.visible {
            self.config.sidebar_width as f32 / scale
        } else {
            0.0
        };
        let terminal_w = logical_w - sidebar_logical;
        let pad = self.config.terminal_padding;

        let cols = if cell_w > 0.0 {
            ((terminal_w - pad * 2.0).max(0.0) / cell_w).max(1.0) as u16
        } else {
            DEFAULT_COLS
        };
        let rows = if cell_h > 0.0 {
            ((logical_h - pad * 2.0).max(0.0) / cell_h).max(1.0) as u16
        } else {
            DEFAULT_ROWS
        };
        let sidebar_cols = if self.sidebar.visible && cell_w > 0.0 {
            (sidebar_logical / cell_w) as u16
        } else {
            0
        };

        let grid = Arc::new(std::sync::RwLock::new(Grid::new(
            rows as usize,
            cols as usize,
            self.config.scrollback_lines,
        )));
        let dirty_rows = Arc::new(DirtyRows::new());
        dirty_rows.mark_all();

        let pty = PtyHandle::spawn(PtySize::new(cols, rows))?;
        let mut pty_reader = PtyReader::new()?;
        pty_reader.register(pty.raw_fd())?;

        let vt_parser = VtParser::new();
        let vt_state = TerminalState::new(rows, cols);

        let wid = cockpit_window.window.id();

        Ok((
            wid,
            TerminalWindow {
                cockpit_window,
                renderer,
                grid,
                dirty_rows,
                pty,
                pty_reader,
                vt_parser,
                vt_state,
                grid_cols: cols,
                grid_rows: rows,
                sidebar_cols,
                sidebar_rendered_version: 0,
                claude_session: None,
                custom_title: None,
                renaming_tab: false,
                title_edit_buffer: String::new(),
                last_set_title: String::new(),
                scroll_offset: 0,
            },
        ))
    }

    fn start_pollers(&mut self) {
        if self.pollers_started {
            return;
        }
        self.pollers_started = true;

        let usage_interval = Duration::from_secs(self.config.poll_usage_secs);
        self.usage_rx = Some(usage::poller::spawn(&self.rt_handle, usage_interval));

        let sessions_interval = Duration::from_secs(self.config.poll_sessions_secs);
        let (req_tx, res_rx) = discovery::poller::spawn(&self.rt_handle, sessions_interval);
        self.sessions_req_tx = Some(req_tx);
        self.sessions_rx = Some(res_rx);
    }

    fn drain_pollers(&mut self) {
        let mut changed = false;

        if let Some(ref mut rx) = self.usage_rx {
            while let Ok(update) = rx.try_recv() {
                match update {
                    usage::UsageUpdate::Data { account_name, data } => {
                        let found = self
                            .sidebar
                            .accounts
                            .iter_mut()
                            .find(|a| a.account_name == account_name);
                        if let Some(existing) = found {
                            existing.data = data;
                        } else {
                            self.sidebar.accounts.push(
                                crate::sidebar::state::AccountUsage {
                                    account_name,
                                    data,
                                },
                            );
                        }
                        self.sidebar.usage_error = None;
                        changed = true;
                    }
                    usage::UsageUpdate::Error(e) => {
                        self.sidebar.usage_error = Some(e);
                        changed = true;
                    }
                }
            }
        }

        // Push current tab list to poller
        if let Some(ref tx) = self.sessions_req_tx {
            let requests: Vec<_> = self
                .windows
                .iter()
                .map(|(_, tw)| discovery::TabScanRequest {
                    shell_pid: tw.pty.child_pid().as_raw() as u32,
                })
                .collect();
            let _ = tx.send(requests);
        }

        // Drain session results and update per-tab ClaudeSession
        if let Some(ref mut rx) = self.sessions_rx {
            while let Ok(results) = rx.try_recv() {
                for result in results {
                    let pid = result.shell_pid;
                    for (_, tw) in &mut self.windows {
                        if tw.pty.child_pid().as_raw() as u32 == pid {
                            if tw.claude_session != result.session {
                                tw.claude_session = result.session;
                                changed = true;
                            }
                            break;
                        }
                    }
                }
            }
        }

        // Rebuild tab entries for sidebar
        if changed {
            self.sidebar.tab_entries = self.build_tab_entries();
            self.sidebar_version += 1;
        }
    }

    fn build_tab_entries(&self) -> Vec<TabSessionEntry> {
        self.windows
            .iter()
            .enumerate()
            .map(|(i, (_, tw))| TabSessionEntry {
                tab_index: i,
                display_title: Self::resolve_tab_title(tw, i),
                session: tw.claude_session.clone(),
            })
            .collect()
    }

    fn resolve_tab_title(tw: &TerminalWindow, index: usize) -> String {
        // 1. Custom title
        if let Some(ref title) = tw.custom_title {
            return title.clone();
        }
        // 2. Claude session topic
        if let Some(ref session) = tw.claude_session {
            if !session.topic.is_empty() {
                return session.topic.chars().take(40).collect();
            }
        }
        // 3. VT window title
        if !tw.vt_state.window_title.is_empty() {
            return tw.vt_state.window_title.clone();
        }
        // 4. Default
        format!("Tab {}", index + 1)
    }

    fn find_window_idx(&self, window_id: WindowId) -> Option<usize> {
        self.windows.iter().position(|(id, _)| *id == window_id)
    }

    /// Render a specific window. Uses struct destructuring for split borrows.
    #[allow(clippy::too_many_lines)]
    fn render_window(&mut self, idx: usize) -> Result<()> {
        // Destructure self to get disjoint mutable/immutable borrows
        let Self {
            windows,
            sidebar,
            sidebar_version,
            config,
            active_tab,
            cursor_blink_visible,
            selection,
            ..
        } = self;

        let (_, tw) = match windows.get_mut(idx) {
            Some(pair) => pair,
            None => return Ok(()),
        };

        let cols = tw.grid_cols as u32;
        let rows = tw.grid_rows as u32;

        // Drain dirty rows
        let dirty_mask = tw.dirty_rows.drain();
        let terminal_dirty = dirty_mask != [0u64; 8];
        let sidebar_dirty = tw.sidebar_rendered_version != *sidebar_version;

        if !terminal_dirty && !sidebar_dirty {
            return Ok(());
        }

        let renderer = &mut tw.renderer;
        let phys_size = tw.cockpit_window.window.inner_size();
        let scale = tw.cockpit_window.window.scale_factor() as f32;
        let vp_w = phys_size.width as f32 / scale;
        let vp_h = phys_size.height as f32 / scale;

        let atlas_w = renderer.atlas.atlas_width();
        let atlas_h = renderer.atlas.atlas_height();

        // Update terminal uniforms with padding on all edges (logical coords)
        let pad = config.terminal_padding;
        renderer.update_uniforms(cols, rows, vp_w, vp_h, pad, pad);

        // Write dirty terminal rows to GPU buffer
        if terminal_dirty {
            let grid = tw
                .grid
                .read()
                .map_err(|e| CockpitError::Render(format!("grid lock poisoned: {e}")))?;

            // Cursor state for this frame (only show cursor at live position)
            let scroll_off = tw.scroll_offset;
            let cursor = &tw.vt_state.cursor;
            let show_cursor = scroll_off == 0 && cursor.visible && *cursor_blink_visible;
            let cursor_row = cursor.row as usize;
            let cursor_col = cursor.col as usize;

            let cell_ptr = renderer.cell_buffer_ptr();
            let cell_capacity = renderer.ctx.cell_capacity;
            for (bucket_idx, bucket) in dirty_mask.iter().enumerate() {
                let mut bit = *bucket;
                while bit != 0 {
                    let row_idx = (bucket_idx * 64) + bit.trailing_zeros() as usize;
                    bit &= bit - 1;

                    if row_idx >= rows as usize {
                        continue;
                    }

                    let row_offset = row_idx * (cols as usize);
                    for col_idx in 0..cols as usize {
                        let offset = row_offset + col_idx;
                        if offset >= cell_capacity {
                            break;
                        }
                        let mut gpu_cell = match grid.cell_scrolled(row_idx, col_idx, scroll_off) {
                            Some(cell) => {
                                let fg = resolve_color(&config.colors, &cell.fg, true);
                                let bg = resolve_color(&config.colors, &cell.bg, false);
                                let flags = cell.flags.bits() as u32;

                                let glyph_key = GlyphKey {
                                    ch: cell.ch,
                                    bold: cell.flags.contains(CellFlags::BOLD),
                                    italic: cell.flags.contains(CellFlags::ITALIC),
                                };
                                let (uv_x, uv_y, uv_w, uv_h) =
                                    match renderer.atlas.get_or_insert(glyph_key) {
                                        Ok(entry) => (
                                            entry.x as f32 / atlas_w,
                                            entry.y as f32 / atlas_h,
                                            entry.width as f32 / atlas_w,
                                            entry.height as f32 / atlas_h,
                                        ),
                                        Err(_) => (0.0, 0.0, 0.0, 0.0),
                                    };

                                GpuCell {
                                    glyph_index: cell.ch as u32,
                                    fg_color: fg,
                                    bg_color: bg,
                                    flags,
                                    atlas_uv_x: uv_x,
                                    atlas_uv_y: uv_y,
                                    atlas_uv_w: uv_w,
                                    atlas_uv_h: uv_h,
                                }
                            }
                            None => GpuCell::default(),
                        };

                        // Block cursor: swap fg/bg at cursor position
                        if show_cursor && row_idx == cursor_row && col_idx == cursor_col {
                            std::mem::swap(&mut gpu_cell.fg_color, &mut gpu_cell.bg_color);
                        }

                        // Selection highlight: swap fg/bg for selected cells
                        if let Some(sel) = selection {
                            if sel.contains(row_idx, col_idx) {
                                std::mem::swap(&mut gpu_cell.fg_color, &mut gpu_cell.bg_color);
                            }
                        }

                        // SAFETY: offset < cell_capacity checked above.
                        unsafe {
                            cell_ptr.add(offset).write(gpu_cell);
                        }
                    }
                }
            }
            drop(grid);
        }

        // Sidebar
        let draw_sidebar = sidebar.visible && tw.sidebar_cols > 0;
        let sb_cols = tw.sidebar_cols as u32;
        let sb_rows = rows;

        let (sb_cols, sb_rows) = if draw_sidebar {
            if sidebar_dirty {
                let (sb_cols, sb_rows, resize_ok) =
                    if let Err(e) = renderer.resize_sidebar(sb_cols, sb_rows) {
                        tracing::error!("Failed to resize sidebar buffer: {e}");
                        let clamped = TerminalRenderer::clamp_sidebar_dims(
                            sb_cols,
                            sb_rows,
                            renderer.ctx.sidebar_cell_capacity,
                        );
                        (clamped.0, clamped.1, false)
                    } else {
                        (sb_cols, sb_rows, true)
                    };

                // Use clamped dimensions for rendering to prevent GPU OOB
                let render_cols = sb_cols as u16;
                let render_rows = sb_rows as u16;
                let mut hit_map = Vec::new();
                let sidebar_cells = layout::render_sidebar(
                    sidebar,
                    render_cols,
                    render_rows,
                    *active_tab,
                    &mut renderer.atlas,
                    &mut hit_map,
                );
                sidebar.hit_map = hit_map;

                renderer.upload_sidebar_cells(&sidebar_cells);
                // Only mark sidebar as clean if resize succeeded;
                // otherwise force re-render next frame with correct dims.
                if resize_ok {
                    tw.sidebar_rendered_version = *sidebar_version;
                }
                (sb_cols, sb_rows)
            } else {
                (sb_cols, sb_rows)
            }
        } else {
            (sb_cols, sb_rows)
        };

        if draw_sidebar {
            let sidebar_x = vp_w - config.sidebar_width as f32 / scale;
            renderer.update_sidebar_uniforms(
                sb_cols,
                sb_rows,
                vp_w,
                vp_h,
                sidebar_x,
                pad,
            );
        }

        // Issue draw
        let bg = &config.colors.background;
        let cell_capacity = renderer.ctx.cell_capacity;
        let capped_rows = if cols > 0 {
            let max_rows = cell_capacity / (cols as usize);
            rows.min(max_rows as u32)
        } else {
            rows
        };

        renderer.render_frame(
            &tw.cockpit_window.metal_layer,
            cols,
            capped_rows,
            [bg[0] as f64, bg[1] as f64, bg[2] as f64, bg[3] as f64],
            sb_cols,
            sb_rows,
            draw_sidebar,
        )
    }

    fn handle_resize_window(&mut self, idx: usize) {
        let Self {
            windows,
            config,
            sidebar,
            ..
        } = self;

        let (_, tw) = match windows.get_mut(idx) {
            Some(pair) => pair,
            None => return,
        };

        let cell_w = tw.renderer.atlas.cell_width;
        let cell_h = tw.renderer.atlas.cell_height;
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }

        let phys_size = tw.cockpit_window.window.inner_size();
        let scale = tw.cockpit_window.window.scale_factor() as f32;
        let logical_w = phys_size.width as f32 / scale;
        let logical_h = phys_size.height as f32 / scale;

        let sidebar_logical = if sidebar.visible {
            config.sidebar_width as f32 / scale
        } else {
            0.0
        };
        let terminal_w = logical_w - sidebar_logical;
        let pad = config.terminal_padding;

        let new_cols = ((terminal_w - pad * 2.0).max(0.0) / cell_w) as u16;
        let new_rows = ((logical_h - pad * 2.0).max(0.0) / cell_h) as u16;
        let new_sidebar_cols = if sidebar.visible {
            (sidebar_logical / cell_w) as u16
        } else {
            0
        };

        if new_cols == 0 || new_rows == 0 {
            return;
        }

        let changed = new_cols != tw.grid_cols
            || new_rows != tw.grid_rows
            || new_sidebar_cols != tw.sidebar_cols;

        if !changed {
            return;
        }

        // Grow GPU buffers if needed — if this fails, skip the resize entirely
        // to avoid the grid having larger dimensions than the GPU buffer.
        if let Err(e) = tw.renderer.resize(new_cols as u32, new_rows as u32) {
            tracing::error!("Failed to resize GPU buffer: {e}");
            return;
        }
        if new_sidebar_cols > 0 {
            if let Err(e) = tw
                .renderer
                .resize_sidebar(new_sidebar_cols as u32, new_rows as u32)
            {
                tracing::error!("Failed to resize sidebar GPU buffer: {e}");
            }
        }

        tw.resize(new_cols, new_rows);
        tw.sidebar_cols = new_sidebar_cols;
        // Force sidebar re-render on next frame
        tw.sidebar_rendered_version = 0;

        tw.cockpit_window
            .metal_layer
            .setContentsScale(scale as f64);
        tw.cockpit_window
            .metal_layer
            .setDrawableSize(objc2_foundation::NSSize::new(
                phys_size.width as f64,
                phys_size.height as f64,
            ));
    }

    fn toggle_sidebar(&mut self) {
        self.sidebar.toggle_visibility();
        // Trigger resize on all windows
        for i in 0..self.windows.len() {
            self.handle_resize_window(i);
        }
    }

    fn adjust_font_size(&mut self, window_idx: usize, delta: f32) {
        const DEFAULT_SIZE: f32 = 14.0;
        const MIN_SIZE: f32 = 8.0;
        const MAX_SIZE: f32 = 72.0;

        let new_size = if delta == 0.0 {
            DEFAULT_SIZE
        } else {
            (self.config.font_size + delta).clamp(MIN_SIZE, MAX_SIZE)
        };

        if (new_size - self.config.font_size).abs() < 0.01 {
            return;
        }

        self.config.font_size = new_size;

        // Rebuild only the glyph atlas — keep MetalContext (device, pipeline,
        // buffers) intact to avoid buffer size/state mismatches.
        let (_, tw) = match self.windows.get_mut(window_idx) {
            Some(pair) => pair,
            None => return,
        };

        let atlas = match GlyphAtlas::new(
            &tw.renderer.ctx.device,
            &self.config.font_family,
            self.config.font_size,
            1.2,
        ) {
            Ok(a) => a,
            Err(e) => {
                tracing::error!("Failed to recreate glyph atlas: {e}");
                return;
            }
        };

        tw.renderer.atlas = atlas;
        tw.dirty_rows.mark_all();
        tw.sidebar_rendered_version = 0;

        self.handle_resize_window(window_idx);
    }

    /// Paste clipboard contents into the PTY, with bracketed paste support.
    fn paste_clipboard(&self, window_idx: usize) {
        let text = match clipboard_text() {
            Some(t) if !t.is_empty() => t,
            _ => return,
        };

        let (_, tw) = match self.windows.get(window_idx) {
            Some(pair) => pair,
            None => return,
        };

        if tw.vt_state.bracketed_paste {
            tw.write_pty(b"\x1b[200~");
        }
        tw.write_pty(text.as_bytes());
        if tw.vt_state.bracketed_paste {
            tw.write_pty(b"\x1b[201~");
        }
    }

    /// Extract selected text from the grid.
    fn extract_selection_text(
        &self,
        grid: &crate::grid::storage::SharedGrid,
        sel: &Selection,
        scroll_offset: usize,
    ) -> String {
        let grid = match grid.read() {
            Ok(g) => g,
            Err(_) => return String::new(),
        };
        let (r0, c0, r1, c1) = sel.ordered();
        let mut result = String::new();
        for row in r0..=r1 {
            let col_start = if row == r0 { c0 } else { 0 };
            let col_end = if row == r1 {
                c1 + 1
            } else {
                grid.cols()
            };
            for col in col_start..col_end {
                if let Some(cell) = grid.cell_scrolled(row, col, scroll_offset) {
                    if cell.ch != '\0' && cell.ch != ' ' {
                        result.push(cell.ch);
                    } else {
                        result.push(' ');
                    }
                }
            }
            // Trim trailing spaces on each line
            let trimmed = result.trim_end_matches(' ');
            result.truncate(trimmed.len());
            if row < r1 {
                result.push('\n');
            }
        }
        result
    }
}

/// Read the current macOS clipboard (pasteboard) as a string.
fn clipboard_text() -> Option<String> {
    use objc2::runtime::AnyClass;
    use objc2::msg_send;
    use objc2_foundation::NSString;

    unsafe {
        let cls = AnyClass::get(c"NSPasteboard")?;
        let pb: *mut objc2::runtime::AnyObject = msg_send![cls, generalPasteboard];
        if pb.is_null() {
            return None;
        }
        let ns_string_type = NSString::from_str("public.utf8-plain-text");
        let result: *mut objc2::runtime::AnyObject =
            msg_send![pb, stringForType: &*ns_string_type];
        if result.is_null() {
            return None;
        }
        let ns_str: &NSString = &*(result as *const NSString);
        Some(ns_str.to_string())
    }
}

/// Write a string to the macOS clipboard (pasteboard).
fn set_clipboard_text(text: &str) {
    use objc2::runtime::AnyClass;
    use objc2::msg_send;
    use objc2_foundation::NSString;

    unsafe {
        let cls = match AnyClass::get(c"NSPasteboard") {
            Some(c) => c,
            None => return,
        };
        let pb: *mut objc2::runtime::AnyObject = msg_send![cls, generalPasteboard];
        if pb.is_null() {
            return;
        }
        let _: isize = msg_send![pb, clearContents];
        let ns_string = NSString::from_str(text);
        let _: bool = msg_send![pb, setString: &*ns_string, forType: &*NSString::from_str("public.utf8-plain-text")];
    }
}

/// Set up the native macOS Window menu. Once registered via `setWindowsMenu:`,
/// macOS automatically populates it with "Merge All Windows", "Move Tab to
/// New Window", the window list, and other tab management items.
fn setup_macos_window_menu() {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Sel};
    use objc2_foundation::NSString;

    unsafe {
        let app_cls = match AnyClass::get(c"NSApplication") {
            Some(c) => c,
            None => return,
        };
        let ns_app: *const AnyObject = msg_send![app_cls, sharedApplication];
        if ns_app.is_null() {
            return;
        }

        let main_menu: *const AnyObject = msg_send![&*ns_app, mainMenu];
        if main_menu.is_null() {
            return;
        }

        let menu_cls = match AnyClass::get(c"NSMenu") {
            Some(c) => c,
            None => return,
        };
        let item_cls = match AnyClass::get(c"NSMenuItem") {
            Some(c) => c,
            None => return,
        };

        // Create "Window" menu
        let title = NSString::from_str("Window");
        let window_menu: *mut AnyObject = msg_send![menu_cls, alloc];
        let window_menu: *mut AnyObject = msg_send![window_menu, initWithTitle: &*title];

        // Minimize (Cmd+M)
        let min_title = NSString::from_str("Minimize");
        let min_key = NSString::from_str("m");
        let min_action = Sel::register(c"performMiniaturize:");
        let _: *mut AnyObject = msg_send![
            &*window_menu,
            addItemWithTitle: &*min_title,
            action: min_action,
            keyEquivalent: &*min_key
        ];

        // Zoom
        let zoom_title = NSString::from_str("Zoom");
        let empty = NSString::from_str("");
        let zoom_action = Sel::register(c"performZoom:");
        let _: *mut AnyObject = msg_send![
            &*window_menu,
            addItemWithTitle: &*zoom_title,
            action: zoom_action,
            keyEquivalent: &*empty
        ];

        // Separator
        let separator: *mut AnyObject = msg_send![item_cls, separatorItem];
        let _: () = msg_send![&*window_menu, addItem: &*separator];

        // "Merge All Windows"
        let merge_title = NSString::from_str("Merge All Windows");
        let merge_action = Sel::register(c"mergeAllWindows:");
        let _: *mut AnyObject = msg_send![
            &*window_menu,
            addItemWithTitle: &*merge_title,
            action: merge_action,
            keyEquivalent: &*empty
        ];

        // Wrap in a menu bar item and add to main menu
        let container: *mut AnyObject = msg_send![item_cls, alloc];
        let container: *mut AnyObject = msg_send![container, init];
        let _: () = msg_send![&*container, setSubmenu: &*window_menu];
        let _: () = msg_send![&*main_menu, addItem: &*container];

        // Register as the Window menu — macOS auto-adds window list,
        // "Show Tab Bar", "Move Tab to New Window", etc.
        let _: () = msg_send![&*ns_app, setWindowsMenu: &*window_menu];
    }
}

fn resolve_color(colors: &crate::config::ColorScheme, color: &Color, is_fg: bool) -> [f32; 4] {
    match color {
        Color::Default => {
            if is_fg {
                colors.foreground
            } else {
                colors.background
            }
        }
        Color::Named(named) => {
            let idx = match named {
                NamedColor::Black => 0,
                NamedColor::Red => 1,
                NamedColor::Green => 2,
                NamedColor::Yellow => 3,
                NamedColor::Blue => 4,
                NamedColor::Magenta => 5,
                NamedColor::Cyan => 6,
                NamedColor::White => 7,
                NamedColor::BrightBlack => 8,
                NamedColor::BrightRed => 9,
                NamedColor::BrightGreen => 10,
                NamedColor::BrightYellow => 11,
                NamedColor::BrightBlue => 12,
                NamedColor::BrightMagenta => 13,
                NamedColor::BrightCyan => 14,
                NamedColor::BrightWhite => 15,
            };
            colors
                .ansi
                .get(idx)
                .copied()
                .unwrap_or(colors.foreground)
        }
        Color::Indexed(idx) => {
            if (*idx as usize) < 16 {
                colors
                    .ansi
                    .get(*idx as usize)
                    .copied()
                    .unwrap_or(colors.foreground)
            } else {
                let c = *idx;
                if c < 232 {
                    let c = c - 16;
                    let r = c / 36;
                    let g = (c % 36) / 6;
                    let b = c % 6;
                    [
                        if r == 0 { 0.0 } else { (55.0 + 40.0 * r as f32) / 255.0 },
                        if g == 0 { 0.0 } else { (55.0 + 40.0 * g as f32) / 255.0 },
                        if b == 0 { 0.0 } else { (55.0 + 40.0 * b as f32) / 255.0 },
                        1.0,
                    ]
                } else {
                    let v = (8.0 + 10.0 * (c - 232) as f32) / 255.0;
                    [v, v, v, 1.0]
                }
            }
        }
        Color::Rgb(r, g, b) => [*r as f32 / 255.0, *g as f32 / 255.0, *b as f32 / 255.0, 1.0],
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if !self.windows.is_empty() {
            return;
        }

        match self.create_terminal_window(event_loop) {
            Ok((wid, tw)) => {
                self.windows.push((wid, tw));
            }
            Err(e) => {
                tracing::error!("Failed to create initial window: {e}");
                event_loop.exit();
                return;
            }
        }

        // Register the macOS Window menu — enables "Merge All Windows",
        // "Move Tab to New Window", "Show Tab Bar", and the window list.
        setup_macos_window_menu();

        self.start_pollers();
    }

    #[allow(clippy::too_many_lines)]
    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let idx = match self.find_window_idx(window_id) {
            Some(i) => i,
            None => return,
        };

        match event {
            WindowEvent::Focused(true) => {
                if self.active_tab != idx {
                    self.active_tab = idx;
                    self.sidebar.dirty = true;
                    self.sidebar_version += 1;
                }
            }
            WindowEvent::CloseRequested => {
                self.windows.remove(idx);
                self.sidebar.tab_entries = self.build_tab_entries();
                self.sidebar_version += 1;
                if self.active_tab >= self.windows.len() && !self.windows.is_empty() {
                    self.active_tab = self.windows.len() - 1;
                }
                if self.windows.is_empty() {
                    event_loop.exit();
                }
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.render_window(idx) {
                    tracing::error!("Render failed: {e}");
                }
            }
            WindowEvent::Resized(_) => {
                self.handle_resize_window(idx);
            }
            WindowEvent::ModifiersChanged(new_mods) => {
                self.modifiers = new_mods.state();
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                // Reset cursor blink on any keypress so it stays visible while typing
                self.cursor_blink_visible = true;
                self.cursor_blink_last_toggle = Instant::now();

                let super_key = self.modifiers.super_key();
                let shift_key = self.modifiers.shift_key();

                if super_key {
                    match &event.logical_key {
                        // Font scaling
                        Key::Character(s) if s.as_str() == "+" || s.as_str() == "=" => {
                            self.adjust_font_size(idx, 1.0);
                            return;
                        }
                        Key::Character(s) if s.as_str() == "-" => {
                            self.adjust_font_size(idx, -1.0);
                            return;
                        }
                        Key::Character(s) if s.as_str() == "0" => {
                            self.adjust_font_size(idx, 0.0);
                            return;
                        }
                        // Sidebar: Cmd+B toggle, Cmd+Shift+B cycle panel
                        Key::Character(s) if s.as_str() == "b" || s.as_str() == "B" => {
                            if shift_key {
                                let next = match self.sidebar.panel {
                                    crate::sidebar::state::SidebarPanel::Sessions => {
                                        crate::sidebar::state::SidebarPanel::Usage
                                    }
                                    crate::sidebar::state::SidebarPanel::Usage => {
                                        crate::sidebar::state::SidebarPanel::Output
                                    }
                                    crate::sidebar::state::SidebarPanel::Output => {
                                        crate::sidebar::state::SidebarPanel::Sessions
                                    }
                                };
                                self.sidebar.switch_panel(next);
                                self.sidebar_version += 1;
                            } else {
                                self.toggle_sidebar();
                            }
                            return;
                        }
                        // Cmd+Shift+R: enter/exit rename mode
                        Key::Character(s)
                            if (s.as_str() == "r" || s.as_str() == "R") && shift_key =>
                        {
                            let mut title_changed = false;
                            if let Some((_, tw)) = self.windows.get_mut(self.active_tab) {
                                if tw.renaming_tab {
                                    // Already renaming — commit
                                    if !tw.title_edit_buffer.is_empty() {
                                        tw.custom_title = Some(tw.title_edit_buffer.clone());
                                    }
                                    tw.renaming_tab = false;
                                    tw.title_edit_buffer.clear();
                                    tw.last_set_title.clear();
                                    title_changed = true;
                                } else {
                                    // Start renaming
                                    tw.renaming_tab = true;
                                    tw.title_edit_buffer.clear();
                                    tw.cockpit_window.window.set_title("Rename: _");
                                }
                            }
                            if title_changed {
                                self.sidebar.tab_entries = self.build_tab_entries();
                                self.sidebar_version += 1;
                            }
                            return;
                        }
                        // Cmd+C: copy selection if active, otherwise send SIGINT
                        Key::Character(s) if s.as_str() == "c" => {
                            if let Some(sel) = &self.selection {
                                // Copy selected text to clipboard
                                if let Some((_, tw)) = self.windows.get(idx) {
                                    let text = self.extract_selection_text(
                                        &tw.grid, sel, tw.scroll_offset,
                                    );
                                    if !text.is_empty() {
                                        set_clipboard_text(&text);
                                    }
                                }
                                // Keep selection visible after copy
                                if let Some((_, tw)) = self.windows.get(idx) {
                                    tw.dirty_rows.mark_all();
                                }
                            } else {
                                if let Some((_, tw)) = self.windows.get(idx) {
                                    tw.write_pty(&[0x03]); // ETX = Ctrl+C
                                }
                            }
                            return;
                        }
                        // Cmd+V: paste from clipboard into PTY
                        Key::Character(s) if s.as_str() == "v" => {
                            self.paste_clipboard(idx);
                            return;
                        }
                        // Cmd+T: new tab (new native window, macOS groups as tab)
                        Key::Character(s) if s.as_str() == "t" => {
                            match self.create_terminal_window(event_loop) {
                                Ok((wid, tw)) => {
                                    self.windows.push((wid, tw));
                                    self.sidebar.tab_entries = self.build_tab_entries();
                                    self.sidebar_version += 1;
                                }
                                Err(e) => {
                                    tracing::error!("Failed to create tab: {e}");
                                }
                            }
                            return;
                        }
                        // Cmd+W: close this window/tab
                        Key::Character(s) if s.as_str() == "w" => {
                            self.windows.remove(idx);
                            self.sidebar.tab_entries = self.build_tab_entries();
                            self.sidebar_version += 1;
                            if self.windows.is_empty() {
                                event_loop.exit();
                            }
                            return;
                        }
                        // Cmd+N: new window (same as Cmd+T for tabbed mode)
                        Key::Character(s) if s.as_str() == "n" => {
                            match self.create_terminal_window(event_loop) {
                                Ok((wid, tw)) => {
                                    self.windows.push((wid, tw));
                                    self.sidebar.tab_entries = self.build_tab_entries();
                                    self.sidebar_version += 1;
                                }
                                Err(e) => {
                                    tracing::error!("Failed to create window: {e}");
                                }
                            }
                            return;
                        }
                        // Cmd+1-9: switch to tab N (via native macOS tab API)
                        Key::Character(s) => {
                            if let Some(digit) = s.as_str().chars().next() {
                                if digit.is_ascii_digit() && digit != '0' {
                                    let tab_idx = (digit as usize) - ('1' as usize);
                                    if let Some((_, tw)) = self.windows.get(idx) {
                                        tw.cockpit_window.window.select_tab_at_index(tab_idx);
                                    }
                                    return;
                                }
                            }
                            // Let unhandled Cmd+key combos (Cmd+Q, Cmd+H, Cmd+C,
                            // etc.) fall through to the system.
                            return;
                        }
                        _ => {
                            // Let other Cmd+ combos pass through to system
                            return;
                        }
                    }
                }

                // Ctrl+Tab / Ctrl+Shift+Tab: next/prev tab (native macOS tab API)
                if self.modifiers.control_key() {
                    if let Key::Named(NamedKey::Tab) = &event.logical_key {
                        if let Some((_, tw)) = self.windows.get(idx) {
                            if shift_key {
                                tw.cockpit_window.window.select_previous_tab();
                            } else {
                                tw.cockpit_window.window.select_next_tab();
                            }
                        }
                        return;
                    }
                }

                // Rename mode — intercept keys instead of sending to PTY
                {
                    let mut title_changed = false;
                    let mut in_rename = false;
                    if let Some((_, tw)) = self.windows.get_mut(self.active_tab) {
                        if tw.renaming_tab {
                            in_rename = true;
                            match &event.logical_key {
                                Key::Named(NamedKey::Enter) => {
                                    // Commit rename
                                    if !tw.title_edit_buffer.is_empty() {
                                        tw.custom_title = Some(tw.title_edit_buffer.clone());
                                    }
                                    tw.renaming_tab = false;
                                    tw.title_edit_buffer.clear();
                                    tw.last_set_title.clear();
                                    title_changed = true;
                                }
                                Key::Named(NamedKey::Escape) => {
                                    // Cancel rename
                                    tw.renaming_tab = false;
                                    tw.title_edit_buffer.clear();
                                    tw.last_set_title.clear();
                                    title_changed = true;
                                }
                                Key::Named(NamedKey::Backspace) => {
                                    tw.title_edit_buffer.pop();
                                    let display = if tw.title_edit_buffer.is_empty() {
                                        "Rename: _".to_string()
                                    } else {
                                        format!("Rename: {}_", tw.title_edit_buffer)
                                    };
                                    tw.cockpit_window.window.set_title(&display);
                                }
                                Key::Named(NamedKey::Space) => {
                                    tw.title_edit_buffer.push(' ');
                                    let display = format!("Rename: {}_", tw.title_edit_buffer);
                                    tw.cockpit_window.window.set_title(&display);
                                }
                                Key::Character(s) => {
                                    tw.title_edit_buffer.push_str(s.as_str());
                                    let display = format!("Rename: {}_", tw.title_edit_buffer);
                                    tw.cockpit_window.window.set_title(&display);
                                }
                                _ => {}
                            }
                        }
                    }
                    if title_changed {
                        self.sidebar.tab_entries = self.build_tab_entries();
                        self.sidebar_version += 1;
                    }
                    if in_rename {
                        return;
                    }
                }

                // Normal key — send to PTY
                let bytes: Option<&[u8]> = match &event.logical_key {
                    Key::Character(s) => Some(s.as_bytes()),
                    Key::Named(named) => {
                        let app_cursor = self
                            .windows
                            .get(idx)
                            .map(|(_, tw)| tw.vt_state.application_cursor_keys)
                            .unwrap_or(false);
                        named_key_bytes(named, app_cursor)
                    }
                    _ => None,
                };

                if let Some(bytes) = bytes {
                    if let Some((_, tw)) = self.windows.get_mut(idx) {
                        // Snap to live on keypress
                        if tw.scroll_offset > 0 {
                            tw.scroll_offset = 0;
                            tw.dirty_rows.mark_all();
                        }
                        tw.write_pty(bytes);
                    }
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor_x = position.x;
                self.cursor_y = position.y;

                // Update text selection while dragging
                if self.selecting {
                    if let Some((_, tw)) = self.windows.get(idx) {
                        let scale = tw.cockpit_window.window.scale_factor();
                        let (row, col) = pixel_to_grid(
                            position.x,
                            position.y,
                            tw.renderer.atlas.cell_width,
                            tw.renderer.atlas.cell_height,
                            self.config.terminal_padding,
                            scale,
                            tw.grid_cols,
                            tw.grid_rows,
                        );
                        if let Some(ref mut sel) = self.selection {
                            if sel.end_row != row || sel.end_col != col {
                                sel.end_row = row;
                                sel.end_col = col;
                                tw.dirty_rows.mark_all();
                            }
                        }
                    }
                }

                if self.dragging_sidebar {
                    if let Some((_, tw)) = self.windows.get(idx) {
                        let size = tw.cockpit_window.window.inner_size();
                        let new_width = (size.width as f64 - position.x)
                            .max(100.0)
                            .min(size.width as f64 * 0.6) as u32;
                        if new_width != self.config.sidebar_width {
                            self.config.sidebar_width = new_width;
                            // Resize all windows to reflect new sidebar width
                            for i in 0..self.windows.len() {
                                self.handle_resize_window(i);
                            }
                        }
                    }
                } else if self.sidebar.visible {
                    if let Some((_, tw)) = self.windows.get(idx) {
                        let width = tw.cockpit_window.window.inner_size().width;
                        let edge_x = width.saturating_sub(self.config.sidebar_width) as f64;
                        if (position.x - edge_x).abs() < 5.0 {
                            tw.cockpit_window.window.set_cursor(CursorIcon::ColResize);
                        } else if position.x > edge_x {
                            // Inside sidebar — detect hover over session cards
                            tw.cockpit_window.window.set_cursor(CursorIcon::Pointer);
                            let cell_h = tw.renderer.atlas.cell_height as f64;
                            if cell_h > 0.0 {
                                let sidebar_row = ((position.y - self.config.terminal_padding as f64).max(0.0) / cell_h) as u16;
                                let new_hovered = self
                                    .sidebar
                                    .hit_map
                                    .iter()
                                    .find(|entry| {
                                        sidebar_row >= entry.start_row
                                            && sidebar_row < entry.end_row
                                    })
                                    .map(|entry| entry.tab_index);
                                if new_hovered != self.sidebar.hovered_tab {
                                    self.sidebar.hovered_tab = new_hovered;
                                    self.sidebar.dirty = true;
                                    self.sidebar_version += 1;
                                }
                            }
                        } else {
                            tw.cockpit_window.window.set_cursor(CursorIcon::Default);
                            if self.sidebar.hovered_tab.is_some() {
                                self.sidebar.hovered_tab = None;
                                self.sidebar.dirty = true;
                                self.sidebar_version += 1;
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == winit::event::MouseButton::Left {
                    // Determine click zone: sidebar edge, sidebar interior, or terminal
                    let win_info = self.windows.get(idx).map(|(_, tw)| {
                        let width = tw.cockpit_window.window.inner_size().width;
                        let cell_h = tw.renderer.atlas.cell_height as f64;
                        let cell_w = tw.renderer.atlas.cell_width;
                        let scale = tw.cockpit_window.window.scale_factor();
                        let cols = tw.grid_cols;
                        let rows = tw.grid_rows;
                        (width, cell_h, cell_w, scale, cols, rows)
                    });
                    if let Some((width, cell_h, cell_w, scale, cols, rows)) = win_info {
                        let edge_x = if self.sidebar.visible {
                            width.saturating_sub(self.config.sidebar_width) as f64
                        } else {
                            width as f64
                        };
                        let in_sidebar_edge = (self.cursor_x - edge_x).abs() < 5.0;
                        let in_sidebar = self.sidebar.visible && self.cursor_x > edge_x;

                        match state {
                            ElementState::Pressed if in_sidebar_edge && self.sidebar.visible => {
                                self.dragging_sidebar = true;
                            }
                            ElementState::Pressed if in_sidebar => {
                                // Click inside sidebar — switch to hovered tab,
                                // double-click enters rename mode.
                                if cell_h > 0.0 {
                                    let sidebar_row = ((self.cursor_y - self.config.terminal_padding as f64).max(0.0) / cell_h) as u16;
                                    let clicked_tab = self
                                        .sidebar
                                        .hit_map
                                        .iter()
                                        .find(|e| {
                                            sidebar_row >= e.start_row
                                                && sidebar_row < e.end_row
                                        })
                                        .map(|e| e.tab_index);

                                    if let Some(tab_idx) = clicked_tab {
                                        let now = Instant::now();
                                        let is_double_click = self
                                            .last_sidebar_click_time
                                            .zip(self.last_sidebar_click_tab)
                                            .is_some_and(|(t, prev_tab)| {
                                                prev_tab == tab_idx
                                                    && now.duration_since(t)
                                                        < Duration::from_millis(400)
                                            });

                                        if is_double_click {
                                            self.last_sidebar_click_time = None;
                                            self.last_sidebar_click_tab = None;
                                            self.active_tab = tab_idx;
                                            self.sidebar.dirty = true;
                                            self.sidebar_version += 1;
                                            if let Some((_, tw)) =
                                                self.windows.get_mut(tab_idx)
                                            {
                                                tw.renaming_tab = true;
                                                tw.title_edit_buffer.clear();
                                                tw.cockpit_window
                                                    .window
                                                    .set_title("Rename: _");
                                            }
                                        } else {
                                            self.last_sidebar_click_time = Some(now);
                                            self.last_sidebar_click_tab = Some(tab_idx);
                                            // Switch to the clicked tab: update
                                            // active_tab immediately for sidebar
                                            // highlight, then tell macOS to switch
                                            // the native tab.
                                            if self.active_tab != tab_idx {
                                                self.active_tab = tab_idx;
                                                self.sidebar.dirty = true;
                                                self.sidebar_version += 1;
                                            }
                                            if let Some((_, tw)) = self.windows.get(idx)
                                            {
                                                tw.cockpit_window
                                                    .window
                                                    .select_tab_at_index(tab_idx);
                                            }
                                        }
                                    }
                                }
                            }
                            ElementState::Pressed => {
                                // Click in terminal area — start text selection
                                let (row, col) = pixel_to_grid(
                                    self.cursor_x,
                                    self.cursor_y,
                                    cell_w,
                                    cell_h as f32,
                                    self.config.terminal_padding,
                                    scale,
                                    cols,
                                    rows,
                                );
                                self.selection = Some(Selection {
                                    start_row: row,
                                    start_col: col,
                                    end_row: row,
                                    end_col: col,
                                });
                                self.selecting = true;
                                // Mark all dirty to clear previous selection highlight
                                if let Some((_, tw)) = self.windows.get(idx) {
                                    tw.dirty_rows.mark_all();
                                }
                            }
                            ElementState::Released => {
                                self.dragging_sidebar = false;
                                if self.selecting {
                                    self.selecting = false;
                                    // If start == end (click without drag), clear selection
                                    // and attempt click-to-position cursor movement.
                                    if let Some(ref sel) = self.selection {
                                        if sel.start_row == sel.end_row
                                            && sel.start_col == sel.end_col
                                        {
                                            let click_row = sel.start_row;
                                            let click_col = sel.start_col;
                                            self.selection = None;
                                            if let Some((_, tw)) = self.windows.get(idx) {
                                                tw.dirty_rows.mark_all();
                                                // Click-to-position: if not scrolled
                                                // and click is on the cursor's row,
                                                // emit arrow keys to reposition.
                                                if tw.scroll_offset == 0 {
                                                    let cursor_row =
                                                        tw.vt_state.cursor.row as usize;
                                                    let cursor_col =
                                                        tw.vt_state.cursor.col as usize;
                                                    if click_row == cursor_row
                                                        && click_col != cursor_col
                                                    {
                                                        let app_cursor = tw
                                                            .vt_state
                                                            .application_cursor_keys;
                                                        let (seq, count) =
                                                            if click_col > cursor_col {
                                                                let s: &[u8] = if app_cursor
                                                                {
                                                                    b"\x1bOC"
                                                                } else {
                                                                    b"\x1b[C"
                                                                };
                                                                (
                                                                    s,
                                                                    click_col - cursor_col,
                                                                )
                                                            } else {
                                                                let s: &[u8] = if app_cursor
                                                                {
                                                                    b"\x1bOD"
                                                                } else {
                                                                    b"\x1b[D"
                                                                };
                                                                (
                                                                    s,
                                                                    cursor_col - click_col,
                                                                )
                                                            };
                                                        for _ in 0..count {
                                                            tw.write_pty(seq);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let lines = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => -y as isize * 3,
                    winit::event::MouseScrollDelta::PixelDelta(pos) => {
                        let cell_h = self
                            .windows
                            .get(idx)
                            .map(|(_, tw)| tw.renderer.atlas.cell_height as f64)
                            .unwrap_or(20.0);
                        (-pos.y / cell_h) as isize
                    }
                };

                if let Some((_, tw)) = self.windows.get_mut(idx) {
                    let max_offset = tw
                        .grid
                        .read()
                        .map(|g| g.scrollback_len())
                        .unwrap_or(0);

                    let new_offset = if lines < 0 {
                        // Scroll up (into history)
                        tw.scroll_offset
                            .saturating_add((-lines) as usize)
                            .min(max_offset)
                    } else {
                        // Scroll down (toward live)
                        tw.scroll_offset.saturating_sub(lines as usize)
                    };

                    if new_offset != tw.scroll_offset {
                        tw.scroll_offset = new_offset;
                        tw.dirty_rows.mark_all();
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Reap any zombie child processes from previously-dropped PTYs
        crate::pty::reap_zombies();

        // Poll all windows' PTYs and update titles
        let mut any_pty_data = false;
        for (i, (_, tw)) in self.windows.iter_mut().enumerate() {
            if tw.poll_pty() {
                any_pty_data = true;
            }

            // Don't overwrite the "Rename: ..." display during rename mode
            if !tw.renaming_tab {
                let title = Self::resolve_tab_title(tw, i);
                if title != tw.last_set_title {
                    tw.cockpit_window.window.set_title(&title);
                    tw.last_set_title = title;
                }
            }
        }

        // Drain async pollers
        self.drain_pollers();

        // Check for dead shells — close the window if shell exited
        let mut dead_indices: Vec<usize> = Vec::new();
        for (i, (_, tw)) in self.windows.iter().enumerate() {
            if !tw.pty.is_alive() {
                dead_indices.push(i);
            }
        }
        // Remove dead windows in reverse order to preserve indices
        if !dead_indices.is_empty() {
            for &i in dead_indices.iter().rev() {
                self.windows.remove(i);
            }
            // Fix active_tab to prevent OOB after removal
            if !self.windows.is_empty() {
                if self.active_tab >= self.windows.len() {
                    self.active_tab = self.windows.len() - 1;
                }
            }
            self.sidebar.tab_entries = self.build_tab_entries();
            self.sidebar_version += 1;
            if self.windows.is_empty() {
                event_loop.exit();
                return;
            }
        }

        // Cursor blink: toggle every 500ms
        if self.cursor_blink_last_toggle.elapsed() >= Duration::from_millis(500) {
            self.cursor_blink_visible = !self.cursor_blink_visible;
            self.cursor_blink_last_toggle = Instant::now();
            // Mark cursor row dirty on the active window
            if let Some((_, tw)) = self.windows.get(self.active_tab) {
                let row = tw.vt_state.cursor.row as usize;
                tw.dirty_rows.mark(row as u16);
            }
        }

        // Request redraw on dirty windows
        let sidebar_ver = self.sidebar_version;
        for (_, tw) in &self.windows {
            let has_dirty = tw.dirty_rows.any_dirty();
            let sidebar_stale = tw.sidebar_rendered_version != sidebar_ver;
            if has_dirty || sidebar_stale {
                tw.cockpit_window.window.request_redraw();
            }
        }

        // Adaptive control flow: spin fast when PTY data is flowing, idle at ~60fps otherwise.
        let frame_result = if any_pty_data {
            FrameResult::DataReceived
        } else {
            FrameResult::Idle
        };
        match event_policy::next_wait_duration(frame_result) {
            Some(dur) => event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + dur)),
            None => event_loop.set_control_flow(ControlFlow::Poll),
        }
    }
}

fn named_key_bytes(key: &NamedKey, app_cursor: bool) -> Option<&'static [u8]> {
    let bytes: &'static [u8] = match key {
        NamedKey::Space => b" ",
        NamedKey::Enter => b"\r",
        NamedKey::Tab => b"\t",
        NamedKey::Backspace => b"\x7f",
        NamedKey::Escape => b"\x1b",
        NamedKey::ArrowUp if app_cursor => b"\x1bOA",
        NamedKey::ArrowDown if app_cursor => b"\x1bOB",
        NamedKey::ArrowRight if app_cursor => b"\x1bOC",
        NamedKey::ArrowLeft if app_cursor => b"\x1bOD",
        NamedKey::ArrowUp => b"\x1b[A",
        NamedKey::ArrowDown => b"\x1b[B",
        NamedKey::ArrowRight => b"\x1b[C",
        NamedKey::ArrowLeft => b"\x1b[D",
        NamedKey::Home => b"\x1b[H",
        NamedKey::End => b"\x1b[F",
        NamedKey::PageUp => b"\x1b[5~",
        NamedKey::PageDown => b"\x1b[6~",
        NamedKey::Insert => b"\x1b[2~",
        NamedKey::Delete => b"\x1b[3~",
        NamedKey::F1 => b"\x1bOP",
        NamedKey::F2 => b"\x1bOQ",
        NamedKey::F3 => b"\x1bOR",
        NamedKey::F4 => b"\x1bOS",
        NamedKey::F5 => b"\x1b[15~",
        NamedKey::F6 => b"\x1b[17~",
        NamedKey::F7 => b"\x1b[18~",
        NamedKey::F8 => b"\x1b[19~",
        NamedKey::F9 => b"\x1b[20~",
        NamedKey::F10 => b"\x1b[21~",
        NamedKey::F11 => b"\x1b[23~",
        NamedKey::F12 => b"\x1b[24~",
        _ => return None,
    };
    Some(bytes)
}
