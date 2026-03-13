use std::sync::Arc;

use crate::grid::storage::Grid;
use crate::primitives::DirtyRows;

use super::handler::{bulk_print_char, VtHandler};
use super::state::TerminalState;

/// SIMD-accelerated VT parser wrapper.
///
/// Tracks whether the VTE state machine is in Ground state. The SIMD fast path
/// (bulk ASCII print) is only entered when in Ground — otherwise continuation
/// bytes from a mid-escape sequence could be misinterpreted as printable text.
pub struct VtParser {
    vte_parser: vte::Parser,
    /// True when the VTE parser is known to be in Ground state (no partial
    /// escape sequence in progress). Starts true (parser initializes to Ground).
    in_ground: bool,
}

impl VtParser {
    pub fn new() -> Self {
        Self {
            vte_parser: vte::Parser::new(),
            in_ground: true,
        }
    }

    /// Process a byte buffer through the VT parser, writing to the grid.
    ///
    /// Uses NEON SIMD on aarch64 to fast-path pure printable ASCII chunks,
    /// bypassing the VTE state machine — but only when we know the parser is
    /// in Ground state.
    pub fn process(
        &mut self,
        bytes: &[u8],
        grid: &mut Grid,
        state: &mut TerminalState,
        dirty: &Arc<DirtyRows>,
    ) {
        let len = bytes.len();
        let mut offset = 0;

        while offset < len {
            let remaining = len - offset;

            // SIMD fast path: only when in Ground state and chunk is all printable ASCII
            if self.in_ground && remaining >= 16 {
                if let Some(chunk) = bytes.get(offset..offset + 16) {
                    if all_printable_16(chunk) {
                        for &b in chunk {
                            bulk_print_char(grid, state, dirty, b as char);
                        }
                        offset += 16;
                        // Still in Ground — printable ASCII doesn't change VTE state
                        continue;
                    }
                }
            }

            // Slow path: feed through VTE state machine.
            // Process one byte at a time when mid-escape so we can detect
            // return-to-ground as early as possible. Process in chunks when
            // in ground to find where the escape starts.
            let end = if self.in_ground {
                // Scan for first non-printable byte to find escape boundary
                let chunk_end = (offset + 16).min(len);
                let mut esc_start = chunk_end;
                for idx in offset..chunk_end {
                    let b = bytes.get(idx).copied().unwrap_or(0);
                    if !(0x20..=0x7E).contains(&b) {
                        esc_start = idx;
                        break;
                    }
                }
                if esc_start > offset {
                    // Process the printable prefix through VTE (will call print)
                    if let Some(chunk) = bytes.get(offset..esc_start) {
                        let mut handler = VtHandler::new(grid, state, dirty);
                        self.vte_parser.advance(&mut handler, chunk);
                    }
                    offset = esc_start;
                    if offset >= len {
                        break;
                    }
                }
                // Now process the non-printable byte(s) — this starts an escape
                self.in_ground = false;
                (offset + 1).min(len)
            } else {
                // Mid-escape: feed one byte at a time so we can check for
                // ground-return after each dispatch callback
                (offset + 1).min(len)
            };

            if let Some(chunk) = bytes.get(offset..end) {
                let mut handler = GroundTrackingHandler {
                    inner: VtHandler::new(grid, state, dirty),
                    returned_to_ground: false,
                };
                self.vte_parser.advance(&mut handler, chunk);
                if handler.returned_to_ground {
                    self.in_ground = true;
                }
            }
            offset = end;
        }
    }
}

/// Wrapper around VtHandler that detects when VTE dispatches a complete
/// sequence (returning the state machine to Ground).
struct GroundTrackingHandler<'a> {
    inner: VtHandler<'a>,
    returned_to_ground: bool,
}

impl<'a> vte::Perform for GroundTrackingHandler<'a> {
    fn print(&mut self, c: char) {
        self.inner.print(c);
        // print() is only called from Ground state
        self.returned_to_ground = true;
    }

    fn execute(&mut self, byte: u8) {
        self.inner.execute(byte);
        // execute() returns to Ground
        self.returned_to_ground = true;
    }

    fn csi_dispatch(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, action: char) {
        self.inner.csi_dispatch(params, intermediates, ignore, action);
        // CSI dispatch returns to Ground
        self.returned_to_ground = true;
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        self.inner.esc_dispatch(intermediates, ignore, byte);
        // ESC dispatch returns to Ground
        self.returned_to_ground = true;
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        self.inner.osc_dispatch(params, bell_terminated);
        // OSC dispatch returns to Ground
        self.returned_to_ground = true;
    }

    fn hook(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, action: char) {
        self.inner.hook(params, intermediates, ignore, action);
        // hook enters DCS passthrough — NOT ground
        self.returned_to_ground = false;
    }

    fn unhook(&mut self) {
        self.inner.unhook();
        // unhook returns to Ground
        self.returned_to_ground = true;
    }

    fn put(&mut self, byte: u8) {
        self.inner.put(byte);
        // put is DCS data — still in passthrough, NOT ground
    }
}

/// Check if all 16 bytes are printable ASCII (0x20..=0x7E) using NEON SIMD.
#[cfg(target_arch = "aarch64")]
fn all_printable_16(bytes: &[u8]) -> bool {
    if bytes.len() < 16 {
        return false;
    }
    // SAFETY: We've verified bytes.len() >= 16, and we're on aarch64.
    // vld1q_u8 reads 16 bytes from the pointer, which is within bounds.
    unsafe {
        use std::arch::aarch64::*;
        let chunk = vld1q_u8(bytes.as_ptr());
        let ge_space = vcgeq_u8(chunk, vdupq_n_u8(0x20));
        let le_tilde = vcleq_u8(chunk, vdupq_n_u8(0x7E));
        let in_range = vandq_u8(ge_space, le_tilde);
        vminvq_u8(in_range) == 0xFF
    }
}

/// Fallback for non-aarch64: byte-by-byte check.
#[cfg(not(target_arch = "aarch64"))]
fn all_printable_16(bytes: &[u8]) -> bool {
    if bytes.len() < 16 {
        return false;
    }
    bytes.get(..16).map_or(false, |chunk| {
        chunk.iter().all(|&b| (0x20..=0x7E).contains(&b))
    })
}

impl Default for VtParser {
    fn default() -> Self {
        Self::new()
    }
}
