use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Mutex;

use nix::libc;
use nix::pty::openpty;
use nix::sys::signal::{self, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag};
use nix::unistd::{close, dup2, execvp, fork, setsid, ForkResult, Pid};

use crate::error::{CockpitError, Result};

// ---------------------------------------------------------------------------
// Global zombie reaper
// ---------------------------------------------------------------------------

/// PIDs awaiting reap. `Drop` pushes here; the event loop calls `reap_zombies()`.
static REAPER: Mutex<Vec<Pid>> = Mutex::new(Vec::new());

/// Non-blocking reap of all tracked PIDs.
///
/// Call this from the event loop (e.g. `about_to_wait`). It does a non-blocking
/// `waitpid` on every PID in the list and removes those that have exited.
pub fn reap_zombies() {
    let Ok(mut pids) = REAPER.lock() else {
        return; // poisoned — nothing we can do in a safe way
    };
    pids.retain(|&pid| {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(nix::sys::wait::WaitStatus::StillAlive) => true, // keep watching
            Ok(_) => false,  // exited / signaled — reap complete
            Err(_) => false, // ECHILD etc. — already gone
        }
    });
}

/// Return a snapshot of the PIDs currently tracked by the reaper.
/// Useful for testing.
pub fn reaper_pids() -> Vec<Pid> {
    let Ok(pids) = REAPER.lock() else {
        return Vec::new();
    };
    pids.clone()
}

#[derive(Debug, Clone, Copy)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

impl PtySize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            pixel_width: 0,
            pixel_height: 0,
        }
    }

    fn to_winsize(self) -> nix::pty::Winsize {
        nix::pty::Winsize {
            ws_row: self.rows,
            ws_col: self.cols,
            ws_xpixel: self.pixel_width,
            ws_ypixel: self.pixel_height,
        }
    }
}

pub struct PtyHandle {
    master_fd: OwnedFd,
    child_pid: Pid,
    size: PtySize,
}

impl PtyHandle {
    /// Spawn a new PTY with the user's shell.
    pub fn spawn(size: PtySize) -> Result<Self> {
        let winsize = size.to_winsize();

        let pty = openpty(Some(&winsize), None)
            .map_err(|e| CockpitError::Pty(format!("openpty failed: {e}")))?;

        let master_fd = pty.master;
        let slave_fd = pty.slave;

        // Pre-allocate all strings in the parent BEFORE fork.
        // After fork, the child must only call async-signal-safe functions.
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let c_shell = CString::new(shell.as_bytes())
            .map_err(|e| CockpitError::Pty(format!("invalid shell path: {e}")))?;
        let base_name = shell.rsplit('/').next().unwrap_or("zsh");
        let login_name = CString::new(format!("-{base_name}"))
            .map_err(|e| CockpitError::Pty(format!("invalid login name: {e}")))?;
        // Pre-allocate env var name for unsetenv in child
        let claudecode_env = CString::new("CLAUDECODE")
            .map_err(|e| CockpitError::Pty(format!("env var name: {e}")))?;

        // SAFETY: fork() is unsafe because child must not use non-async-signal-safe
        // functions before exec. The child below only calls setsid, ioctl, dup2,
        // close, and execvp — all async-signal-safe.
        match unsafe { fork() } {
            Ok(ForkResult::Child) => {
                // Child process — all code paths must diverge (_exit or exec).
                // All heap allocations (CString) were done in the parent above.

                // Close master in child
                drop(master_fd);

                // Create new session
                if setsid().is_err() {
                    unsafe { libc::_exit(1) };
                }

                // Set controlling terminal (TIOCSCTTY)
                // SAFETY: slave_fd is a valid fd from openpty
                unsafe {
                    let ret = libc::ioctl(slave_fd.as_raw_fd(), u64::from(libc::TIOCSCTTY), 0);
                    if ret < 0 {
                        libc::_exit(1);
                    }
                }

                // Dup2 slave to stdin/stdout/stderr
                let slave_raw = slave_fd.as_raw_fd();
                if dup2(slave_raw, 0).is_err()
                    || dup2(slave_raw, 1).is_err()
                    || dup2(slave_raw, 2).is_err()
                {
                    unsafe { libc::_exit(1) };
                }

                // Close original slave fd if it's not one of 0,1,2
                if slave_raw > 2 {
                    let _ = close(slave_raw);
                }
                // Prevent OwnedFd from double-closing
                std::mem::forget(slave_fd);

                // Unset CLAUDECODE so Claude Code can launch inside this terminal.
                // unsetenv is async-signal-safe.
                unsafe { libc::unsetenv(claudecode_env.as_ptr()) };

                // execvp replaces the process; if it returns, it failed
                let _ = execvp(&c_shell, &[login_name]);
                unsafe { libc::_exit(1) }
            }
            Ok(ForkResult::Parent { child }) => {
                // Parent process: close slave
                drop(slave_fd);

                // Set master to non-blocking
                let flags = nix::fcntl::fcntl(master_fd.as_raw_fd(), nix::fcntl::FcntlArg::F_GETFL)
                    .map_err(|e| CockpitError::Pty(format!("fcntl getfl: {e}")))?;
                let mut oflags = nix::fcntl::OFlag::from_bits_truncate(flags);
                oflags.insert(nix::fcntl::OFlag::O_NONBLOCK);
                nix::fcntl::fcntl(
                    master_fd.as_raw_fd(),
                    nix::fcntl::FcntlArg::F_SETFL(oflags),
                )
                .map_err(|e| CockpitError::Pty(format!("fcntl setfl: {e}")))?;

                // SAFETY: master_fd is a valid fd from openpty, owned by parent
                let owned_master = unsafe { OwnedFd::from_raw_fd(master_fd.as_raw_fd()) };
                // Prevent the original from double-closing
                std::mem::forget(master_fd);

                Ok(Self {
                    master_fd: owned_master,
                    child_pid: child,
                    size,
                })
            }
            Err(e) => Err(CockpitError::Pty(format!("fork failed: {e}"))),
        }
    }

    /// Resize the PTY.
    pub fn resize(&mut self, size: PtySize) -> Result<()> {
        let ws = size.to_winsize();
        // SAFETY: master_fd is a valid pty master fd
        let ret = unsafe {
            libc::ioctl(self.master_fd.as_raw_fd(), libc::TIOCSWINSZ, &ws as *const _)
        };
        if ret < 0 {
            return Err(CockpitError::Pty(format!(
                "TIOCSWINSZ failed: {}",
                std::io::Error::last_os_error()
            )));
        }
        self.size = size;
        Ok(())
    }

    /// Write data to the PTY master.
    pub fn write(&self, data: &[u8]) -> Result<usize> {
        nix::unistd::write(&self.master_fd, data)
            .map_err(|e| CockpitError::Pty(format!("write failed: {e}")))
    }

    /// Get the raw file descriptor of the master.
    pub fn raw_fd(&self) -> RawFd {
        self.master_fd.as_raw_fd()
    }

    /// Check if the child process is still alive.
    pub fn is_alive(&self) -> bool {
        match waitpid(self.child_pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(nix::sys::wait::WaitStatus::StillAlive) => true,
            Ok(_) => false, // exited, signaled, etc.
            Err(_) => false,
        }
    }

    /// Get the child PID.
    pub fn child_pid(&self) -> Pid {
        self.child_pid
    }

    /// Get the current size.
    pub fn size(&self) -> PtySize {
        self.size
    }
}

impl Drop for PtyHandle {
    fn drop(&mut self) {
        // Only send SIGHUP if the child is still alive (kill with signal 0 checks).
        // This prevents signaling a reused PID after the child has already been reaped.
        if signal::kill(self.child_pid, None).is_ok() {
            let _ = signal::kill(self.child_pid, Signal::SIGHUP);
        }
        // Hand the PID to the global reaper so the event loop can collect it
        // with non-blocking waitpid on subsequent frames.
        if let Ok(mut pids) = REAPER.lock() {
            pids.push(self.child_pid);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use nix::sys::signal::Signal;
    use nix::unistd::Pid;
    use std::process::Command;

    /// Helper: clear the reaper list so tests don't interfere with each other.
    fn clear_reaper() {
        REAPER.lock().unwrap().clear();
    }

    /// After dropping a PtyHandle, the child PID should appear in the REAPER list.
    #[test]
    fn test_reaper_collects_pid() {
        clear_reaper();

        let handle = PtyHandle::spawn(PtySize::new(80, 24)).unwrap();
        let pid = handle.child_pid();

        // Drop the handle — should push PID into the reaper
        drop(handle);

        let pids = reaper_pids();
        assert!(
            pids.contains(&pid),
            "expected PID {pid} in reaper list, got {pids:?}"
        );

        // Clean up: reap until the child is gone
        for _ in 0..50 {
            reap_zombies();
            if !reaper_pids().contains(&pid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Spawn a process, kill it, add its PID to the reaper, call reap_zombies —
    /// the PID should be removed because the process has exited.
    #[test]
    fn test_reap_zombies_cleans_exited() {
        clear_reaper();

        // Spawn a short-lived child via std::process so we control it directly
        let child = Command::new("sleep").arg("300").spawn().unwrap();
        let pid = Pid::from_raw(child.id() as i32);

        // Kill it immediately
        signal::kill(pid, Signal::SIGKILL).unwrap();

        // Give the kernel a moment to deliver the signal
        std::thread::sleep(std::time::Duration::from_millis(50));

        // Manually push into reaper
        REAPER.lock().unwrap().push(pid);
        assert!(reaper_pids().contains(&pid));

        // Reap — the exited process should be collected
        reap_zombies();

        assert!(
            !reaper_pids().contains(&pid),
            "PID {pid} should have been reaped"
        );
    }

    /// A still-running PID should remain in the reaper list after reap_zombies.
    #[test]
    fn test_reap_zombies_keeps_alive() {
        clear_reaper();

        // Spawn a long-lived child
        let child = Command::new("sleep").arg("300").spawn().unwrap();
        let pid = Pid::from_raw(child.id() as i32);

        REAPER.lock().unwrap().push(pid);

        // Reap — child is alive, so PID should stay
        reap_zombies();

        assert!(
            reaper_pids().contains(&pid),
            "PID {pid} should still be in the reaper (process is alive)"
        );

        // Cleanup: kill and reap
        let _ = signal::kill(pid, Signal::SIGKILL);
        std::thread::sleep(std::time::Duration::from_millis(50));
        reap_zombies();
    }
}
