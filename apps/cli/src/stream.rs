//! Fallible process output.
//!
//! `println!`/`print!` panic when the consumer closes the pipe (`head -1` on a
//! stream is the common case), which produced exit 101 with a Rust panic on
//! stderr. Every write goes through this module instead: a closed pipe is a
//! normal stop (exit 0, no panic), and any other write failure is reported as
//! an internal failure without recursing into stdout.

use std::io::{self, Write};

/// Why a write stopped.
#[derive(Debug)]
pub enum WriteFailure {
    /// The reader closed the pipe. This is a normal streaming termination.
    Closed,
    /// The stream failed for another reason.
    Failed(io::Error),
}

impl WriteFailure {
    pub fn is_closed(&self) -> bool {
        matches!(self, Self::Closed)
    }
}

fn classify(error: io::Error) -> WriteFailure {
    if error.kind() == io::ErrorKind::BrokenPipe {
        WriteFailure::Closed
    } else {
        WriteFailure::Failed(error)
    }
}

/// Write and flush `bytes` to any sink.
pub fn write_all(stream: &mut impl Write, bytes: &[u8]) -> Result<(), WriteFailure> {
    stream.write_all(bytes).map_err(classify)?;
    stream.flush().map_err(classify)
}

struct CountingWriter<'a, W: Write> {
    inner: &'a mut W,
    written: u64,
}

impl<W: Write> Write for CountingWriter<'_, W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Write and flush through [`write_all`] while counting accepted bytes.
///
/// The count lets the benchmark distinguish a partial write (some bytes
/// accepted before the pipe closed or the sink failed) from a failed write
/// that never reached the sink. A successful result always counts every
/// requested byte.
pub fn write_all_counted(stream: &mut impl Write, bytes: &[u8]) -> (Result<(), WriteFailure>, u64) {
    let requested = bytes.len() as u64;
    let mut counter = CountingWriter {
        inner: stream,
        written: 0,
    };
    let result = write_all(&mut counter, bytes);
    let written = if result.is_ok() {
        requested
    } else {
        counter.written
    };
    (result, written)
}

/// Write one line to stdout.
pub fn to_stdout(bytes: &[u8]) -> Result<(), WriteFailure> {
    let stdout = io::stdout();
    let mut lock = stdout.lock();
    write_all(&mut lock, bytes)
}

/// Write one line to stderr.
pub fn to_stderr(bytes: &[u8]) -> Result<(), WriteFailure> {
    let stderr = io::stderr();
    let mut lock = stderr.lock();
    write_all(&mut lock, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sink that fails the way a closed pipe does.
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "closed"))
        }
    }

    struct FailingSink;

    impl Write for FailingSink {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("disk full"))
        }
    }

    #[derive(Default)]
    struct Buffer(Vec<u8>);

    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_closed_pipe_is_reported_as_closed_and_exits_zero() {
        let failure = write_all(&mut ClosedPipe, b"{}\n").unwrap_err();
        assert!(failure.is_closed());
        let written = Err(failure);
        assert_eq!(crate::exit_for(7, &written, true), 0);
        assert_eq!(crate::exit_for(7, &written, false), 7);
    }

    #[test]
    fn other_write_failures_are_internal_failures() {
        let failure = write_all(&mut FailingSink, b"{}\n").unwrap_err();
        assert!(!failure.is_closed());
        let written = Err(failure);
        assert_eq!(crate::exit_for(7, &written, true), 1);
        assert_eq!(crate::exit_for(7, &written, false), 7);
    }

    #[test]
    fn successful_writes_are_complete() {
        let mut buffer = Buffer::default();
        write_all(&mut buffer, b"line\n").unwrap();
        assert_eq!(buffer.0, b"line\n");
    }
}
