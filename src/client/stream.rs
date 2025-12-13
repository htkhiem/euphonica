use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;

/// A wrapper struct that can hold either a TcpStream or a UnixStream,
/// implementing the Read and Write traits.
///
/// This is because mpd::Client takes a stream as a generic parameter
/// which complicates its usage: a TcpStream is needed when connecting
/// to MPD via TCP socket (localhost, IP, etc) while a UnixStream is
/// needed for local socket connection.
/// By using this wrapper struct instead, we sacrifice a (tiny?) bit
/// of runtime performance while getting a simpler MpdWrapper codebase
/// in return.
#[derive(Debug)]
pub struct StreamWrapper {
    tcp_stream: Option<TcpStream>,
    unix_stream: Option<UnixStream>,
}

impl StreamWrapper {
    pub fn new_tcp(stream: TcpStream) -> Self {
        StreamWrapper {
            tcp_stream: Some(stream),
            unix_stream: None, // Ensure UnixStream is None
        }
    }

    pub fn new_unix(stream: UnixStream) -> Self {
        StreamWrapper {
            tcp_stream: None, // Ensure TcpStream is None
            unix_stream: Some(stream),
        }
    }
}

impl Read for StreamWrapper {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // Check if a TcpStream is present and attempt to read from it.
        if let Some(ref mut s) = self.tcp_stream {
            s.read(buf)
        // Check if a UnixStream is present and attempt to read from it.
        } else if let Some(ref mut s) = self.unix_stream {
            s.read(buf)
        } else {
            Err(io::Error::other("Uninitialised StreamWrapper"))
        }
    }
}

impl Write for StreamWrapper {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // Check if a TcpStream is present and attempt to write to it.
        if let Some(ref mut s) = self.tcp_stream {
            s.write(buf)
        // Check if a UnixStream is present and attempt to write to it.
        } else if let Some(ref mut s) = self.unix_stream {
            s.write(buf)
        } else {
            Err(io::Error::other("Uninitialised StreamWrapper"))
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // Check if a TcpStream is present and attempt to flush it.
        if let Some(ref mut s) = self.tcp_stream {
            s.flush()
        // Check if a UnixStream is present and attempt to flush it.
        } else if let Some(ref mut s) = self.unix_stream {
            s.flush()
        } else {
            Err(io::Error::other("Uninitialised StreamWrapper"))
        }
    }
}
