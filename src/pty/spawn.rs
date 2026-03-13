use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

use nix::libc;
use nix::pty::openpty;
use nix::sys::wait::{waitpid, WaitPidFlag};
use nix::unistd::{close, dup2, execvp, fork, setsid, ForkResult, Pid};

use crate::error::{CockpitError, Result};

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
