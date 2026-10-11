use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;

/// A connection that is either a plain TCP socket or a server-side TLS session.
pub enum InboundStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::server::TlsStream<TcpStream>>),
}

/// A connection that is either a plain TCP socket or a client-side TLS session.
pub enum OutboundStream {
    Plain(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

macro_rules! impl_stream {
    ($name:ident) => {
        impl AsyncRead for $name {
            fn poll_read(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &mut ReadBuf<'_>,
            ) -> Poll<io::Result<()>> {
                match self.get_mut() {
                    $name::Plain(stream) => Pin::new(stream).poll_read(cx, buf),
                    $name::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
                }
            }
        }

        impl AsyncWrite for $name {
            fn poll_write(
                self: Pin<&mut Self>,
                cx: &mut Context<'_>,
                buf: &[u8],
            ) -> Poll<io::Result<usize>> {
                match self.get_mut() {
                    $name::Plain(stream) => Pin::new(stream).poll_write(cx, buf),
                    $name::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
                }
            }

            fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                match self.get_mut() {
                    $name::Plain(stream) => Pin::new(stream).poll_flush(cx),
                    $name::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
                }
            }

            fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
                match self.get_mut() {
                    $name::Plain(stream) => Pin::new(stream).poll_shutdown(cx),
                    $name::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
                }
            }
        }
    };
}

impl_stream!(InboundStream);
impl_stream!(OutboundStream);

/// Line/byte oriented reader over a stream. Bytes read past the current line or
/// DATA block stay buffered, which matters because SMTP clients may pipeline
/// commands after the DATA terminator.
pub struct Conn<S> {
    stream: S,
    buf: Vec<u8>,
}

impl<S> Conn<S> {
    pub fn new(stream: S) -> Self {
        Self {
            stream,
            buf: Vec::new(),
        }
    }

    /// True when unread bytes are buffered, i.e. a TLS upgrade must wait.
    pub fn has_buffered(&self) -> bool {
        !self.buf.is_empty()
    }

    pub fn into_inner(self) -> S {
        self.stream
    }

    pub fn stream_mut(&mut self) -> &mut S {
        &mut self.stream
    }
}

impl<S: AsyncRead + Unpin> Conn<S> {
    async fn fill(&mut self) -> io::Result<usize> {
        let mut chunk = [0u8; 8192];
        let read = self.stream.read(&mut chunk).await?;
        self.buf.extend_from_slice(&chunk[..read]);
        Ok(read)
    }

    /// Reads a single CRLF-terminated line, returning it without the terminator.
    /// Returns None on a clean EOF.
    pub async fn read_line(&mut self, max: usize) -> io::Result<Option<Vec<u8>>> {
        loop {
            if let Some(position) = self.buf.iter().position(|&byte| byte == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=position).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Some(line));
            }
            if self.buf.len() > max {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
            }
            if self.fill().await? == 0 {
                if self.buf.is_empty() {
                    return Ok(None);
                }
                let line = std::mem::take(&mut self.buf);
                return Ok(Some(line));
            }
        }
    }

    /// Reads a DATA block up to the CRLF.CRLF terminator, undoing dot-stuffing.
    pub async fn read_data(&mut self, max: usize) -> io::Result<Vec<u8>> {
        self.read_data_block(max).await
    }

    /// Reads exactly `count` bytes, for the literal a protocol announces with
    /// a marker rather than a terminator.
    pub async fn read_exact_n(&mut self, count: usize) -> io::Result<Vec<u8>> {
        while self.buf.len() < count {
            if self.fill().await? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed inside a literal",
                ));
            }
        }
        Ok(self.buf.drain(..count).collect())
    }

    async fn read_data_block(&mut self, max: usize) -> io::Result<Vec<u8>> {
        loop {
            if let Some(body) = split_data_block(&mut self.buf) {
                return Ok(dot_unstuff(&body));
            }
            if self.buf.len() > max {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "message exceeds the maximum size",
                ));
            }
            if self.fill().await? == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "connection closed inside DATA",
                ));
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> Conn<S> {
    pub async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.stream.write_all(bytes).await
    }

    pub async fn flush(&mut self) -> io::Result<()> {
        self.stream.flush().await
    }
}

/// Finds a DATA terminator in the buffer, removing it and returning the body.
fn split_data_block(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    if buf.starts_with(b".\r\n") {
        let body = Vec::new();
        buf.drain(..3);
        return Some(body);
    }
    if let Some(position) = buf.windows(5).position(|window| window == b"\r\n.\r\n") {
        let body = buf[..position].to_vec();
        buf.drain(..position + 5);
        return Some(body);
    }
    None
}

/// Removes the leading dot added by SMTP clients on lines that begin with one.
pub fn dot_unstuff(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len());
    let mut at_line_start = true;
    let mut index = 0;
    while index < body.len() {
        if at_line_start && body[index] == b'.' {
            index += 1;
            if index >= body.len() {
                break;
            }
        }
        let byte = body[index];
        out.push(byte);
        at_line_start = byte == b'\n';
        index += 1;
    }
    out
}

/// Adds the extra dot required on lines that begin with one.
pub fn dot_stuff(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 8);
    let mut at_line_start = true;
    for &byte in body {
        if at_line_start && byte == b'.' {
            out.push(b'.');
        }
        out.push(byte);
        at_line_start = byte == b'\n';
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unstuffs_leading_dots() {
        assert_eq!(dot_unstuff(b"..dot\r\nplain\r\n"), b".dot\r\nplain\r\n");
        assert_eq!(dot_stuff(b".dot\r\n"), b"..dot\r\n");
    }

    #[test]
    fn splits_data_block() {
        let mut buf = b"body\r\n.\r\nleftover".to_vec();
        // The CRLF before the dot belongs to the terminator, not the body.
        assert_eq!(split_data_block(&mut buf).unwrap(), b"body");
        assert_eq!(buf, b"leftover");
    }

    #[test]
    fn splits_empty_data_block() {
        let mut buf = b".\r\nnext".to_vec();
        assert_eq!(split_data_block(&mut buf).unwrap(), b"");
        assert_eq!(buf, b"next");
    }

    #[test]
    fn waits_for_terminator() {
        let mut buf = b"body\r\n".to_vec();
        assert!(split_data_block(&mut buf).is_none());
    }
}
