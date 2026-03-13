use std::sync::Arc;
use std::time::Duration;

use winit::application::ApplicationHandler;
use winit::event::{ElementState, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key, NamedKey};
use winit::window::{WindowAttributes, WindowId};

use crate::config::CockpitConfig;
use crate::error::{CockpitError, Result};
use crate::gpu::context::GpuCell;
use crate::gpu::renderer::TerminalRenderer;
use crate::gpu::window::CockpitWindow;
use crate::grid::cell::{Color, NamedColor};
use crate::grid::storage::{Grid, SharedGrid};
use crate::primitives::DirtyRows;
use crate::pty::reader::PtyReader;
use crate::pty::spawn::{PtyHandle, PtySize};
use crate::sidebar::state::SidebarState;
use crate::vt::parser::VtParser;
use crate::vt::state::TerminalState;

const DEFAULT_COLS: u16 = 120;
const DEFAULT_ROWS: u16 = 40;

pub struct App {
    window: Option<CockpitWindow>,
    renderer: Option<TerminalRenderer>,
    grid: SharedGrid,
    dirty_rows: Arc<DirtyRows>,
    pty: Option<PtyHandle>,
    pty_reader: Option<PtyReader>,
    vt_parser: Option<VtParser>,
    vt_state: Option<TerminalState>,
    sidebar: SidebarState,
    config: CockpitConfig,
    grid_cols: u16,
    grid_rows: u16,
}

impl App {
    pub fn new(config: CockpitConfig) -> Self {
        let grid = Arc::new(std::sync::RwLock::new(Grid::new(
            DEFAULT_ROWS as usize,
            DEFAULT_COLS as usize,
            config.scrollback_lines,
        )));
        let dirty_rows = Arc::new(DirtyRows::new());
        let sidebar = SidebarState::new(&config);

        Self {
            window: None,
            renderer: None,
            grid,
            dirty_rows,
            pty: None,
            pty_reader: None,
            vt_parser: None,
            vt_state: None,
            sidebar,
            config,
            grid_cols: DEFAULT_COLS,
            grid_rows: DEFAULT_ROWS,
        }
    }

    fn init_pty(&mut self) -> Result<()> {
        let size = PtySize::new(self.grid_cols, self.grid_rows);
        let pty = PtyHandle::spawn(size)?;

        let mut reader = PtyReader::new()?;
        reader.register(pty.raw_fd())?;

        self.pty_reader = Some(reader);
        self.pty = Some(pty);
        Ok(())
    }

    /// Poll PTY for output. Returns true if data was read.
    fn poll_pty(&mut self) -> bool {
        let (reader, pty) = match (self.pty_reader.as_mut(), self.pty.as_ref()) {
            (Some(r), Some(p)) => (r, p),
            _ => return false,
        };

        let data = match reader.poll_read(pty.raw_fd(), Duration::from_millis(0)) {
            Ok(d) if !d.is_empty() => d.to_vec(),
            _ => return false,
        };

        let parser = match self.vt_parser.as_mut() {
            Some(p) => p,
            None => return false,
        };
        let vt_state = match self.vt_state.as_mut() {
            Some(s) => s,
            None => return false,
        };

        if let Ok(mut grid) = self.grid.write() {
            parser.process(&data, &mut grid, vt_state, &self.dirty_rows);
        }
        true
    }

    fn render_frame(&mut self) -> Result<()> {
        let renderer = match self.renderer.as_ref() {
            Some(r) => r,
            None => return Ok(()),
        };
        let cockpit_window = match self.window.as_ref() {
            Some(w) => w,
            None => return Ok(()),
        };

        let cols = self.grid_cols as u32;
        let rows = self.grid_rows as u32;

        // [I5] Drain dirty bitmask — clears the atomic so we stop re-rendering
        // unchanged frames. Each bit corresponds to a row (0..255, 4 x u64 buckets).
        let dirty_mask = self.dirty_rows.drain();
        if dirty_mask == [0u64; 4] {
            return Ok(());
        }

        // Update uniforms
        let size = cockpit_window.window.inner_size();
        renderer.update_uniforms(cols, rows, size.width as f32, size.height as f32);

        // [I9] Only write dirty rows to the GPU buffer, not the entire grid.
        let grid = self
            .grid
            .read()
            .map_err(|e| CockpitError::Render(format!("grid lock poisoned: {e}")))?;

        let cell_ptr = renderer.cell_buffer_ptr();
        let cell_capacity = renderer.ctx.cell_capacity;
        for (bucket_idx, bucket) in dirty_mask.iter().enumerate() {
            let mut bit = *bucket;
            while bit != 0 {
                let row_idx = (bucket_idx * 64) + bit.trailing_zeros() as usize;
                bit &= bit - 1; // clear lowest set bit

                if row_idx >= rows as usize {
                    continue;
                }

                let row_offset = row_idx * (cols as usize);
                for col_idx in 0..cols as usize {
                    let offset = row_offset + col_idx;
                    // [C5] Cap writes to cell_capacity to prevent buffer overrun.
                    if offset >= cell_capacity {
                        break;
                    }
                    let gpu_cell = match grid.cell(row_idx, col_idx) {
                        Some(cell) => {
                            let glyph_index = cell.ch as u32;
                            let fg = self.resolve_color(&cell.fg, true);
                            let bg = self.resolve_color(&cell.bg, false);
                            let flags = cell.flags.bits() as u32;
                            GpuCell {
                                glyph_index,
                                fg_color: fg,
                                bg_color: bg,
                                flags,
                                // Atlas UVs are zero until glyph rasterization
                                // populates them via get_or_insert().
                                atlas_uv_x: 0.0,
                                atlas_uv_y: 0.0,
                                atlas_uv_w: 0.0,
                                atlas_uv_h: 0.0,
                            }
                        }
                        None => GpuCell::default(),
                    };
                    // SAFETY: offset < cell_capacity checked above. Cell buffer
                    // is allocated for cell_capacity entries in MetalContext::new().
                    unsafe {
                        cell_ptr.add(offset).write(gpu_cell);
                    }
                }
            }
        }
        drop(grid);

        let bg = &self.config.colors.background;

        // [C5] Cap grid dimensions so cols*rows <= cell_capacity. The renderer
        // computes instance_count = cols*rows internally; we clamp rows to
        // keep total cells within the GPU buffer capacity.
        let capped_rows = if cols > 0 {
            let max_rows = cell_capacity / (cols as usize);
            rows.min(max_rows as u32)
        } else {
            rows
        };

        renderer.render_frame(
            &cockpit_window.metal_layer,
            cols,
            capped_rows,
            [bg[0] as f64, bg[1] as f64, bg[2] as f64, bg[3] as f64],
        )
    }

    fn resolve_color(&self, color: &Color, is_fg: bool) -> [f32; 4] {
        match color {
            Color::Default => {
                if is_fg {
                    self.config.colors.foreground
                } else {
                    self.config.colors.background
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
                self.config
                    .colors
                    .ansi
                    .get(idx)
                    .copied()
                    .unwrap_or(self.config.colors.foreground)
            }
            Color::Indexed(idx) => {
                if (*idx as usize) < 16 {
                    self.config
                        .colors
                        .ansi
                        .get(*idx as usize)
                        .copied()
                        .unwrap_or(self.config.colors.foreground)
                } else {
                    // 256-color: compute approximate RGB
                    let c = *idx;
                    if c < 232 {
                        // 6x6x6 color cube (indices 16-231)
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
                        // Grayscale ramp (indices 232-255)
                        let v = (8.0 + 10.0 * (c - 232) as f32) / 255.0;
                        [v, v, v, 1.0]
                    }
                }
            }
            Color::Rgb(r, g, b) => [*r as f32 / 255.0, *g as f32 / 255.0, *b as f32 / 255.0, 1.0],
        }
    }

    fn write_to_pty(&self, data: &[u8]) {
        if let Some(ref pty) = self.pty {
            let _ = pty.write(data);
        }
    }

    fn handle_resize(&mut self, width: u32, height: u32) {
        let renderer = match self.renderer.as_mut() {
            Some(r) => r,
            None => return,
        };

        let cell_w = renderer.atlas.cell_width;
        let cell_h = renderer.atlas.cell_height;
        if cell_w <= 0.0 || cell_h <= 0.0 {
            return;
        }

        let new_cols = (width as f32 / cell_w) as u16;
        let new_rows = (height as f32 / cell_h) as u16;

        if new_cols == 0 || new_rows == 0 {
            return;
        }
        if new_cols == self.grid_cols && new_rows == self.grid_rows {
            return;
        }

        self.grid_cols = new_cols;
        self.grid_rows = new_rows;

        // Grow the GPU cell buffer if needed
        if let Err(e) = renderer.resize(new_cols as u32, new_rows as u32) {
            tracing::error!("Failed to resize GPU buffer: {e}");
        }

        if let Ok(mut grid) = self.grid.write() {
            grid.resize(new_rows as usize, new_cols as usize);
        }
        if let Some(ref mut vt_state) = self.vt_state {
            vt_state.scroll_region = (0, new_rows);
            vt_state.clamp_cursor(new_rows, new_cols);
        }
        if let Some(ref mut pty) = self.pty {
            let _ = pty.resize(PtySize::new(new_cols, new_rows));
        }

        self.dirty_rows.mark_all();

        if let Some(ref w) = self.window {
            let size = w.window.inner_size();
            w.metal_layer
                .setDrawableSize(objc2_foundation::NSSize::new(
                    size.width as f64,
                    size.height as f64,
                ));
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }

        let attrs = WindowAttributes::default()
            .with_title("Claude Cockpit")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 800.0));

        let window = match event_loop.create_window(attrs) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("Failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        let renderer = match TerminalRenderer::new(self.grid_cols as u32, self.grid_rows as u32) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!("Failed to create renderer: {e}");
                event_loop.exit();
                return;
            }
        };

        let cockpit_window = match CockpitWindow::from_window(window, &renderer.ctx.device) {
            Ok(w) => w,
            Err(e) => {
                tracing::error!("Failed to create cockpit window: {e}");
                event_loop.exit();
                return;
            }
        };

        // Set initial drawable size
        let size = cockpit_window.window.inner_size();
        cockpit_window
            .metal_layer
            .setDrawableSize(objc2_foundation::NSSize::new(
                size.width as f64,
                size.height as f64,
            ));

        self.vt_parser = Some(VtParser::new());
        self.vt_state = Some(TerminalState::new(self.grid_rows, self.grid_cols));

        if let Err(e) = self.init_pty() {
            tracing::error!("Failed to spawn PTY: {e}");
            event_loop.exit();
            return;
        }

        // Compute grid size from window
        let cell_w = renderer.atlas.cell_width;
        let cell_h = renderer.atlas.cell_height;
        if cell_w > 0.0 && cell_h > 0.0 {
            let cols = (size.width as f32 / cell_w) as u16;
            let rows = (size.height as f32 / cell_h) as u16;
            if cols > 0 && rows > 0 {
                self.grid_cols = cols;
                self.grid_rows = rows;
                if let Ok(mut grid) = self.grid.write() {
                    grid.resize(rows as usize, cols as usize);
                }
                if let Some(ref mut vt_state) = self.vt_state {
                    vt_state.scroll_region = (0, rows);
                }
                if let Some(ref mut pty) = self.pty {
                    let _ = pty.resize(PtySize::new(cols, rows));
                }
            }
        }

        self.dirty_rows.mark_all();
        self.renderer = Some(renderer);
        self.window = Some(cockpit_window);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => {
                event_loop.exit();
            }
            WindowEvent::RedrawRequested => {
                if let Err(e) = self.render_frame() {
                    tracing::error!("Render failed: {e}");
                }
            }
            WindowEvent::Resized(size) => {
                self.handle_resize(size.width, size.height);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                if event.state != ElementState::Pressed {
                    return;
                }

                let bytes: Option<Vec<u8>> = match &event.logical_key {
                    Key::Character(s) => Some(s.as_bytes().to_vec()),
                    Key::Named(named) => {
                        let app_cursor = self
                            .vt_state
                            .as_ref()
                            .map(|s| s.application_cursor_keys)
                            .unwrap_or(false);
                        named_key_bytes(named, app_cursor)
                    }
                    _ => None,
                };

                if let Some(bytes) = bytes {
                    self.write_to_pty(&bytes);
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // [C6] Poll PTY for new data. With ControlFlow::Poll (set in main.rs),
        // winit calls about_to_wait continuously. We always request redraw when
        // PTY returned data or rows are dirty, ensuring output never stalls.
        // This trades battery for responsiveness — a terminal must never lag.
        let got_data = self.poll_pty();

        // Check if PTY child is still alive
        if let Some(ref pty) = self.pty {
            if !pty.is_alive() {
                tracing::info!("Shell process exited");
            }
        }

        // Request redraw if we got PTY data or any rows are dirty
        if got_data || self.dirty_rows.any_dirty() {
            if let Some(ref w) = self.window {
                w.window.request_redraw();
            }
        }
    }
}

fn named_key_bytes(key: &NamedKey, app_cursor: bool) -> Option<Vec<u8>> {
    let bytes: &[u8] = match key {
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
    Some(bytes.to_vec())
}
