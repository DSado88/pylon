use std::io::Read;
use std::os::fd::{FromRawFd, RawFd};
use std::time::Duration;

use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};

use crate::error::{CockpitError, Result};

const PTY_TOKEN: Token = Token(0);
const READ_BUF_SIZE: usize = 64 * 1024; // 64KB

pub struct PtyReader {
    poll: Poll,
    buf: Vec<u8>,
    registered: bool,
}

impl PtyReader {
    pub fn new() -> Result<Self> {
        let poll = Poll::new()
            .map_err(|e| CockpitError::Pty(format!("Poll::new failed: {e}")))?;

        Ok(Self {
            poll,
            buf: vec![0u8; READ_BUF_SIZE],
            registered: false,
        })
    }

    /// Register a PTY master fd for readable events.
    pub fn register(&mut self, raw_fd: RawFd) -> Result<()> {
        self.poll
            .registry()
            .register(&mut SourceFd(&raw_fd), PTY_TOKEN, Interest::READABLE)
            .map_err(|e| CockpitError::Pty(format!("register failed: {e}")))?;
        self.registered = true;
        Ok(())
    }

    /// Poll for readable data with a timeout. Returns the bytes read, or an empty
    /// slice if the timeout elapsed with no data.
    ///
    /// Loops on read() until WouldBlock to drain all available data, since kqueue
    /// is edge-triggered and won't re-notify for data already buffered.
    pub fn poll_read(&mut self, raw_fd: RawFd, timeout: Duration) -> Result<&[u8]> {
        if !self.registered {
            return Err(CockpitError::Pty("PtyReader not registered".into()));
        }

        let mut events = Events::with_capacity(1);
        self.poll
            .poll(&mut events, Some(timeout))
            .map_err(|e| CockpitError::Pty(format!("poll failed: {e}")))?;

        for event in &events {
            if event.token() == PTY_TOKEN && event.is_readable() {
                // SAFETY: We're creating a File from a raw fd just for the read call.
                // We use ManuallyDrop to prevent it from closing the fd.
                let mut file = std::mem::ManuallyDrop::new(unsafe {
                    std::fs::File::from_raw_fd(raw_fd)
                });

                // Drain all available data — edge-triggered kqueue won't
                // re-notify for buffered data after a single read.
                let mut total = 0;
                loop {
                    let dest = self.buf.get_mut(total..).unwrap_or(&mut []);
                    if dest.is_empty() {
                        // Buffer full — return what we have
                        break;
                    }
                    match file.read(dest) {
                        Ok(0) => {
                            // EOF
                            if total == 0 {
                                return Ok(&[]);
                            }
                            break;
                        }
                        Ok(n) => {
                            total += n;
                            // Keep reading — there may be more data
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            // No more data available right now
                            break;
                        }
                        Err(e) => {
                            if total > 0 {
                                // Return partial data rather than losing it
                                break;
                            }
                            return Err(CockpitError::Pty(format!("read failed: {e}")));
                        }
                    }
                }

                return Ok(self.buf.get(..total).unwrap_or(&[]));
            }
        }

        // Timeout with no events
        Ok(&[])
    }
}
