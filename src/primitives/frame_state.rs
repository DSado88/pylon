use std::sync::atomic::{AtomicU8, Ordering};

use crate::error::{CockpitError, Result};

/// Phases of the render frame lifecycle.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePhase {
    Clean = 0,
    Dirty = 1,
    Encoding = 2,
    Presented = 3,
}

impl FramePhase {
    fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::Clean),
            1 => Ok(Self::Dirty),
            2 => Ok(Self::Encoding),
            3 => Ok(Self::Presented),
            _ => Err(CockpitError::Render(format!("invalid frame phase: {v}"))),
        }
    }
}

/// CAS-only frame state machine.
///
/// Valid transitions:
/// - Clean -> Dirty
/// - Dirty -> Encoding
/// - Encoding -> Presented
/// - Presented -> Clean
/// - Presented -> Dirty
pub struct AtomicFrameState {
    state: AtomicU8,
}

impl AtomicFrameState {
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(FramePhase::Clean as u8),
        }
    }

    /// Attempt a CAS transition from `from` to `to`.
    /// Returns `Ok(())` on success, or `Err` with the current phase on failure.
    pub fn transition(&self, from: FramePhase, to: FramePhase) -> std::result::Result<(), FramePhase> {
        self.state
            .compare_exchange(from as u8, to as u8, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| ())
            .map_err(|actual| {
                // SAFETY: we only store valid FramePhase discriminants
                match FramePhase::from_u8(actual) {
                    Ok(phase) => phase,
                    // If somehow corrupted, report as Clean (defensive)
                    Err(_) => FramePhase::Clean,
                }
            })
    }

    /// Load the current phase.
    pub fn load(&self) -> FramePhase {
        let raw = self.state.load(Ordering::Acquire);
        match FramePhase::from_u8(raw) {
            Ok(phase) => phase,
            Err(_) => FramePhase::Clean,
        }
    }

    /// Mark as dirty via CAS loop. Only transitions from Clean or Presented.
    /// If currently Encoding, leaves state alone — the renderer will pick up
    /// dirty rows on the next frame.
    pub fn mark_dirty(&self) {
        loop {
            let current = self.state.load(Ordering::Acquire);
            match current {
                v if v == FramePhase::Clean as u8 || v == FramePhase::Presented as u8 => {
                    if self
                        .state
                        .compare_exchange(current, FramePhase::Dirty as u8, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                    {
                        return;
                    }
                    // CAS failed, retry
                }
                // Encoding or already Dirty — nothing to do
                _ => return,
            }
        }
    }
}

impl Default for AtomicFrameState {
    fn default() -> Self {
        Self::new()
    }
}
