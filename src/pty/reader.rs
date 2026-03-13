use std::io::Read;
use std::os::fd::{FromRawFd, RawFd};
use std::time::Duration;

use mio::unix::SourceFd;
use mio::{Events, Interest, Poll, Token};

use crate::error::{CockpitError, Result};

const PTY_TOKEN: Token = Token(0);
const READ_BUF_SIZE: usize = 64 * 1024; // 64KB
/// Max bytes to drain per poll_read call. Caps how long the VT parser blocks
/// the event loop when a program floods output (e.g. `cat /dev/urandom`).
/// Remaining data is picked up on the next frame.
const MAX_BYTES_PER_FRAME: usize = 16 * 1024; // 16KB

pub struct PtyReader {
    poll: Poll,
    buf: Vec<u8>,
    overflow: Vec<u8>,
    registered: bool,
}

impl PtyReader {
    pub fn new() -> Result<Self> {
        let poll = Poll::new()
            .map_err(|e| CockpitError::Pty(format!("Poll::new failed: {e}")))?;

        Ok(Self {
            poll,
            buf: vec![0u8; READ_BUF_SIZE],
            overflow: Vec::new(),
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
    /// Reads up to `MAX_BYTES_PER_FRAME` bytes per call to keep the event loop
    /// responsive. Internally drains ALL available data to EAGAIN so that
    /// edge-triggered kqueue is properly re-armed, stashing any excess in an
    /// overflow buffer that is served on subsequent calls.
    pub fn poll_read(&mut self, raw_fd: RawFd, timeout: Duration) -> Result<&[u8]> {
        if !self.registered {
            return Err(CockpitError::Pty("PtyReader not registered".into()));
        }

        // --- Phase 1: serve from overflow if present ---
        if !self.overflow.is_empty() {
            let drain_len = MAX_BYTES_PER_FRAME.min(self.overflow.len());
            let chunk: Vec<u8> = self.overflow.drain(..drain_len).collect();
            let dest = self.buf.get_mut(..chunk.len()).ok_or_else(|| {
                CockpitError::Pty("buf too small for overflow chunk".into())
            })?;
            dest.copy_from_slice(&chunk);
            return Ok(self.buf.get(..chunk.len()).unwrap_or(&[]));
        }

        // --- Phase 2: poll the fd for new data ---
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

                // Drain ALL available data to EAGAIN. This is critical on macOS
                // where kqueue is edge-triggered: if we stop before EAGAIN, the
                // kernel will NOT re-trigger a readable event and remaining bytes
                // are permanently stranded.
                let mut total = 0;
                loop {
                    let remaining = self.buf.len().saturating_sub(total);
                    if remaining == 0 {
                        // Internal buffer full — spill into overflow
                        let mut spill = vec![0u8; READ_BUF_SIZE];
                        match file.read(&mut spill) {
                            Ok(0) => break,
                            Ok(n) => {
                                self.overflow.extend_from_slice(
                                    spill.get(..n).unwrap_or(&[]),
                                );
                                continue;
                            }
                            Err(e)
                                if e.kind() == std::io::ErrorKind::WouldBlock =>
                            {
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    let dest = match self.buf.get_mut(total..total + remaining) {
                        Some(slice) => slice,
                        None => break,
                    };
                    if dest.is_empty() {
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
                            // Keep reading — drain to EAGAIN
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            // Edge trigger reset — no more data right now
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

                // If we read more than MAX_BYTES_PER_FRAME, move the excess
                // into overflow and only return the capped amount.
                if total > MAX_BYTES_PER_FRAME {
                    let excess = self
                        .buf
                        .get(MAX_BYTES_PER_FRAME..total)
                        .unwrap_or(&[]);
                    // Prepend buf excess before any overflow already spilled
                    let mut new_overflow = excess.to_vec();
                    new_overflow.append(&mut self.overflow);
                    self.overflow = new_overflow;
                    total = MAX_BYTES_PER_FRAME;
                }

                return Ok(self.buf.get(..total).unwrap_or(&[]));
            }
        }

        // Timeout with no events
        Ok(&[])
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::fd::AsRawFd;

    /// Helper: create a non-blocking pipe pair and a PtyReader registered to the read end.
    fn pipe_reader() -> (std::fs::File, RawFd, PtyReader) {
        let (read_fd, write_fd) = nix::unistd::pipe().unwrap();
        let raw_read = read_fd.as_raw_fd();
        let raw_write = write_fd.as_raw_fd();

        // Make the read end non-blocking (required for edge-triggered poll)
        nix::fcntl::fcntl(
            raw_read,
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .unwrap();

        // Wrap the write end in a File for convenient write_all().
        // read_fd's OwnedFd is intentionally leaked — the raw fd stays valid for
        // the lifetime of the test and PtyReader reads via raw fd only.
        let write_file = unsafe { std::fs::File::from_raw_fd(raw_write) };
        std::mem::forget(read_fd);
        std::mem::forget(write_fd);

        let mut reader = PtyReader::new().unwrap();
        reader.register(raw_read).unwrap();

        (write_file, raw_read, reader)
    }

    #[test]
    fn test_overflow_buffer_initialized_empty() {
        let reader = PtyReader::new().unwrap();
        assert!(
            reader.overflow.is_empty(),
            "overflow buffer should be empty on construction"
        );
    }

    #[test]
    fn test_drain_to_eagain_with_overflow() {
        let (mut write_file, raw_read, mut reader) = pipe_reader();

        // Write 48KB — 3x the per-frame limit
        let data_size = MAX_BYTES_PER_FRAME * 3;
        let data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();
        write_file.write_all(&data).unwrap();
        drop(write_file); // close write end so reads eventually hit EOF

        // Collect all data across multiple poll_read calls
        let mut collected = Vec::new();
        for _ in 0..10 {
            let chunk = reader
                .poll_read(raw_read, Duration::from_millis(100))
                .unwrap();
            if chunk.is_empty() {
                break;
            }
            collected.extend_from_slice(chunk);
        }

        assert_eq!(
            collected.len(),
            data_size,
            "all {data_size} bytes must be returned across multiple poll_read calls"
        );
        assert_eq!(collected, data, "data must be returned in order");
    }

    #[test]
    fn test_overflow_served_before_new_poll() {
        let (mut write_file, raw_read, mut reader) = pipe_reader();

        // Write 32KB — 2x the per-frame limit
        let data_size = MAX_BYTES_PER_FRAME * 2;
        let data: Vec<u8> = (0..data_size).map(|i| (i % 256) as u8).collect();
        write_file.write_all(&data).unwrap();

        // First read: should get MAX_BYTES_PER_FRAME bytes and stash the rest
        let chunk1 = reader
            .poll_read(raw_read, Duration::from_millis(100))
            .unwrap();
        assert_eq!(
            chunk1.len(),
            MAX_BYTES_PER_FRAME,
            "first call should return exactly MAX_BYTES_PER_FRAME"
        );
        let chunk1_owned = chunk1.to_vec();

        // The overflow buffer should now contain the remaining data
        assert!(
            !reader.overflow.is_empty(),
            "overflow should contain remaining data after first read"
        );

        // Drop the write end AFTER the first read, so there's nothing new to poll
        drop(write_file);

        // Second read: should return the overflow data WITHOUT needing a poll event
        let chunk2 = reader
            .poll_read(raw_read, Duration::from_millis(100))
            .unwrap();
        assert_eq!(
            chunk2.len(),
            MAX_BYTES_PER_FRAME,
            "second call should return the overflow data"
        );

        let mut all = chunk1_owned;
        all.extend_from_slice(chunk2);
        assert_eq!(all, data, "combined chunks must equal original data");
    }
}
