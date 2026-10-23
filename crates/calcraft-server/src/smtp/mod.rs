//! SMTP client for outgoing mail (RFC 5321 subset): `EHLO`, `STARTTLS`
//! (RFC 3207) or implicit TLS (RFC 8314), `AUTH PLAIN`/`AUTH LOGIN`
//! (RFC 4954) over TLS only, `MAIL FROM`/`RCPT TO`/`DATA` with
//! dot-stuffing, `RSET`, `QUIT`.
//!
//! * Every step has a deadline (`smtp.timeout_secs`): DNS, connect, TLS
//!   handshake, every command and reply.
//! * Replies are parsed with limits ([`calcraft_core::mail::ReplyParser`]:
//!   line length, lines per reply) plus a byte limit per reply; a server
//!   that sends more fails the session.
//! * `STARTTLS` is mandatory in `starttls` mode (no silent downgrade), and
//!   data the server sent before the TLS handshake is refused (command
//!   injection, CVE-2011-0411 style).
//! * Passwords are only sent over TLS, kept in zeroizing buffers and never
//!   logged; server replies are never logged or stored either (they can
//!   contain addresses), only reply codes.
//!
//! The client sends ready messages to one recipient each; the reminder
//! queue uses it today, iMIP (scheduling stage B) can use it later.

use std::{
    io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use calcraft_core::mail::{Extensions, Reply, ReplyParser, dot_stuff, normalize_address};
use rustls::{ClientConfig, pki_types::ServerName};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf},
    net::TcpStream,
};
use tokio_rustls::{TlsConnector, client::TlsStream};
use zeroize::Zeroizing;

use crate::{
    config::{SmtpConfig, SmtpTls},
    password_file::{Password, read_password_file},
};

/// Bytes of one reply (all its lines) accepted.
pub const MAX_REPLY_BYTES: usize = 16 * 1024;

/// Why sending failed. Messages and [`SmtpError::code`] never contain
/// server replies, addresses or credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SmtpError {
    #[error("could not connect to the mail server")]
    Connect,
    #[error("mail server timed out")]
    Timeout,
    #[error("TLS with the mail server failed")]
    Tls,
    #[error("mail server does not offer STARTTLS")]
    NoStartTls,
    #[error("mail server offers no supported authentication (PLAIN, LOGIN)")]
    NoAuth,
    #[error("authentication at the mail server failed (reply {0})")]
    Auth(u16),
    #[error("unexpected or malformed reply from the mail server")]
    Protocol,
    #[error("mail server temporarily refused (reply {0})")]
    Deferred(u16),
    #[error("mail server refused (reply {0})")]
    Rejected(u16),
    #[error("message larger than the mail server accepts")]
    TooLarge,
    #[error("invalid message or address")]
    Message,
}

impl SmtpError {
    /// Worth trying again later: everything but a definite refusal of the
    /// message itself.
    pub fn transient(self) -> bool {
        !matches!(self, Self::Rejected(_) | Self::TooLarge | Self::Message)
    }

    /// Short code for the queue and the user's status (no details).
    pub fn code(self) -> &'static str {
        match self {
            Self::Connect | Self::Timeout | Self::Protocol | Self::Deferred(_) => {
                "smtp_unavailable"
            }
            Self::Tls | Self::NoStartTls => "smtp_tls",
            Self::NoAuth | Self::Auth(_) => "smtp_auth",
            Self::Rejected(_) => "smtp_rejected",
            Self::TooLarge => "too_large",
            Self::Message => "invalid_message",
        }
    }
}

fn io_error(err: &io::Error) -> SmtpError {
    if err.kind() == io::ErrorKind::TimedOut {
        SmtpError::Timeout
    } else {
        SmtpError::Protocol
    }
}

/// What the client needs from `[smtp]`.
#[derive(Clone)]
struct Settings {
    host: String,
    port: u16,
    tls: SmtpTls,
    auth: Option<(String, Password)>,
    helo: String,
    from: String,
    timeout: Duration,
    tls_config: Arc<ClientConfig>,
}

/// SMTP client for one configured server. Cheap to clone.
#[derive(Clone)]
pub struct SmtpClient {
    settings: Arc<Settings>,
}

impl std::fmt::Debug for SmtpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SmtpClient")
            .field("host", &self.settings.host)
            .field("port", &self.settings.port)
            .field("tls", &self.settings.tls)
            .finish_non_exhaustive()
    }
}

impl SmtpClient {
    /// Reads the password file and the trust store. No network access.
    pub fn new(config: &SmtpConfig) -> anyhow::Result<Self> {
        config.validate()?;
        anyhow::ensure!(!config.host.is_empty(), "smtp.host is not set");
        let from = config
            .from_address()
            .ok_or_else(|| anyhow::anyhow!("smtp.from is not a valid address"))?;
        let auth = match (&config.username, &config.password_file) {
            (Some(user), Some(path)) => Some((user.clone(), read_password_file(path)?)),
            _ => None,
        };
        let tls_config = crate::https::tls_config(config.ca_file.as_deref(), false)?;
        Ok(Self {
            settings: Arc::new(Settings {
                host: config.host.clone(),
                port: config.effective_port(),
                tls: config.tls,
                auth,
                helo: config.helo(),
                from,
                timeout: config.timeout(),
                tls_config,
            }),
        })
    }

    /// The same client with another deadline per step (tests; the
    /// configuration allows 5 s at least).
    #[doc(hidden)]
    pub fn with_step_timeout(self, timeout: Duration) -> Self {
        let mut settings = Settings::clone(&self.settings);
        settings.timeout = timeout;
        Self {
            settings: Arc::new(settings),
        }
    }

    /// The envelope sender (`smtp.from`).
    pub fn from(&self) -> &str {
        &self.settings.from
    }

    /// Connects, secures and authenticates a session.
    pub async fn connect(&self) -> Result<Connection, SmtpError> {
        let s = &self.settings;
        let tcp = connect_tcp(s).await?;
        let mut wire = Wire {
            stream: Stream::Plain(tcp),
            buf: Vec::new(),
            timeout: s.timeout,
        };
        if s.tls == SmtpTls::Implicit {
            wire.start_tls(s).await?;
        }
        let greeting = wire.read_reply().await?;
        if greeting.code != 220 {
            return Err(refusal(greeting.code));
        }
        let mut ext = wire.ehlo(&s.helo).await?;
        if s.tls == SmtpTls::Starttls {
            if !ext.starttls {
                return Err(SmtpError::NoStartTls);
            }
            wire.command(b"STARTTLS\r\n").await?;
            let reply = wire.read_reply().await?;
            if reply.code != 220 {
                return Err(SmtpError::NoStartTls);
            }
            // Anything the server sent after its 220 would be read as if it
            // came through TLS.
            if !wire.buf.is_empty() {
                return Err(SmtpError::Protocol);
            }
            wire.start_tls(s).await?;
            ext = wire.ehlo(&s.helo).await?;
        }
        if let Some((user, password)) = &s.auth {
            if !wire.stream.is_tls() {
                return Err(SmtpError::Tls);
            }
            wire.authenticate(&ext, user, password).await?;
        }
        Ok(Connection {
            wire,
            ext,
            from: s.from.clone(),
            broken: false,
        })
    }
}

/// Error for a negative reply where a positive one was needed.
fn refusal(code: u16) -> SmtpError {
    if (500..600).contains(&code) {
        SmtpError::Rejected(code)
    } else if (400..500).contains(&code) {
        SmtpError::Deferred(code)
    } else {
        SmtpError::Protocol
    }
}

async fn connect_tcp(s: &Settings) -> Result<TcpStream, SmtpError> {
    let addrs: Vec<SocketAddr> = tokio::time::timeout(
        s.timeout,
        tokio::net::lookup_host((s.host.as_str(), s.port)),
    )
    .await
    .map_err(|_| SmtpError::Timeout)?
    .map_err(|_| SmtpError::Connect)?
    .take(8)
    .collect();
    let mut last = SmtpError::Connect;
    for addr in addrs {
        // Without TLS only this machine is acceptable, whatever DNS says.
        if s.tls == SmtpTls::None && !addr.ip().is_loopback() {
            continue;
        }
        match tokio::time::timeout(s.timeout, TcpStream::connect(addr)).await {
            Ok(Ok(stream)) => {
                let _ = stream.set_nodelay(true);
                return Ok(stream);
            }
            Ok(Err(_)) => last = SmtpError::Connect,
            Err(_) => last = SmtpError::Timeout,
        }
    }
    Err(last)
}

/// A plain or TLS connection.
enum Stream {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
    /// Between taking the plain stream and the end of the handshake.
    Closed,
}

impl Stream {
    fn is_tls(&self) -> bool {
        matches!(self, Self::Tls(_))
    }
}

fn closed() -> io::Error {
    io::Error::new(io::ErrorKind::NotConnected, "connection closed")
}

impl AsyncRead for Stream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_read(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
            Self::Closed => Poll::Ready(Err(closed())),
        }
    }
}

impl AsyncWrite for Stream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_write(cx, buf),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
            Self::Closed => Poll::Ready(Err(closed())),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_flush(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
            Self::Closed => Poll::Ready(Err(closed())),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Self::Plain(s) => Pin::new(s).poll_shutdown(cx),
            Self::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
            Self::Closed => Poll::Ready(Ok(())),
        }
    }
}

/// Line-oriented I/O with deadlines.
struct Wire {
    stream: Stream,
    /// Received bytes not yet consumed.
    buf: Vec<u8>,
    timeout: Duration,
}

impl Wire {
    async fn start_tls(&mut self, s: &Settings) -> Result<(), SmtpError> {
        let Stream::Plain(tcp) = std::mem::replace(&mut self.stream, Stream::Closed) else {
            return Err(SmtpError::Protocol);
        };
        let name = ServerName::try_from(s.host.trim_start_matches('[').trim_end_matches(']'))
            .map_err(|_| SmtpError::Tls)?
            .to_owned();
        let connector = TlsConnector::from(s.tls_config.clone());
        let tls = tokio::time::timeout(self.timeout, connector.connect(name, tcp))
            .await
            .map_err(|_| SmtpError::Timeout)?
            .map_err(|_| SmtpError::Tls)?;
        self.stream = Stream::Tls(Box::new(tls));
        Ok(())
    }

    async fn read_line(&mut self) -> Result<Vec<u8>, SmtpError> {
        loop {
            if let Some(end) = self.buf.iter().position(|&b| b == b'\n') {
                return Ok(self.buf.drain(..=end).collect());
            }
            if self.buf.len() > calcraft_core::mail::MAX_REPLY_LINE + 2 {
                return Err(SmtpError::Protocol);
            }
            let mut chunk = [0u8; 2048];
            let read = tokio::time::timeout(self.timeout, self.stream.read(&mut chunk))
                .await
                .map_err(|_| SmtpError::Timeout)?
                .map_err(|err| io_error(&err))?;
            if read == 0 {
                return Err(SmtpError::Protocol);
            }
            self.buf.extend_from_slice(&chunk[..read]);
        }
    }

    async fn read_reply(&mut self) -> Result<Reply, SmtpError> {
        let mut parser = ReplyParser::new();
        let mut total = 0;
        loop {
            let line = self.read_line().await?;
            total += line.len();
            if total > MAX_REPLY_BYTES {
                return Err(SmtpError::Protocol);
            }
            if let Some(reply) = parser.push(&line).map_err(|_| SmtpError::Protocol)? {
                return Ok(reply);
            }
        }
    }

    async fn command(&mut self, bytes: &[u8]) -> Result<(), SmtpError> {
        tokio::time::timeout(self.timeout, async {
            self.stream.write_all(bytes).await?;
            self.stream.flush().await
        })
        .await
        .map_err(|_| SmtpError::Timeout)?
        .map_err(|err| io_error(&err))
    }

    /// Sends a command and returns the reply.
    async fn exchange(&mut self, bytes: &[u8]) -> Result<Reply, SmtpError> {
        self.command(bytes).await?;
        self.read_reply().await
    }

    async fn ehlo(&mut self, helo: &str) -> Result<Extensions, SmtpError> {
        let reply = self.exchange(format!("EHLO {helo}\r\n").as_bytes()).await?;
        if reply.code != 250 {
            return Err(refusal(reply.code));
        }
        Ok(Extensions::from_ehlo(&reply))
    }

    async fn authenticate(
        &mut self,
        ext: &Extensions,
        user: &str,
        password: &Password,
    ) -> Result<(), SmtpError> {
        let password = password.expose();
        let reply = if ext.auth_plain {
            let mut plain = Zeroizing::new(Vec::with_capacity(user.len() + password.len() + 2));
            plain.push(0);
            plain.extend_from_slice(user.as_bytes());
            plain.push(0);
            plain.extend_from_slice(password.as_bytes());
            let mut line = Zeroizing::new(String::with_capacity(plain.len().div_ceil(3) * 4 + 16));
            line.push_str("AUTH PLAIN ");
            STANDARD.encode_string(plain.as_slice(), &mut line);
            line.push_str("\r\n");
            self.exchange(line.as_bytes()).await?
        } else if ext.auth_login {
            let reply = self.exchange(b"AUTH LOGIN\r\n").await?;
            if reply.code != 334 {
                return Err(SmtpError::Auth(reply.code));
            }
            let mut line = Zeroizing::new(String::with_capacity(user.len().div_ceil(3) * 4 + 2));
            STANDARD.encode_string(user.as_bytes(), &mut line);
            line.push_str("\r\n");
            let reply = self.exchange(line.as_bytes()).await?;
            if reply.code != 334 {
                return Err(SmtpError::Auth(reply.code));
            }
            let mut line =
                Zeroizing::new(String::with_capacity(password.len().div_ceil(3) * 4 + 2));
            STANDARD.encode_string(password.as_bytes(), &mut line);
            line.push_str("\r\n");
            self.exchange(line.as_bytes()).await?
        } else {
            return Err(SmtpError::NoAuth);
        };
        if reply.code == 235 {
            Ok(())
        } else {
            Err(SmtpError::Auth(reply.code))
        }
    }
}

/// An open, authenticated session.
pub struct Connection {
    wire: Wire,
    ext: Extensions,
    from: String,
    /// An I/O error or a malformed reply: the session cannot be used.
    broken: bool,
}

impl Connection {
    /// Whether further messages can be sent over this session.
    pub fn usable(&self) -> bool {
        !self.broken
    }

    /// Sends one message (CRLF line ends, see
    /// [`calcraft_core::mail::Message::render`]) to one recipient.
    pub async fn send(&mut self, to: &str, message: &[u8]) -> Result<(), SmtpError> {
        if self.broken {
            return Err(SmtpError::Protocol);
        }
        let to = normalize_address(to).ok_or(SmtpError::Message)?;
        let data = dot_stuff(message).map_err(|_| SmtpError::Message)?;
        if self
            .ext
            .size
            .is_some_and(|size| size > 0 && message.len() as u64 > size)
        {
            return Err(SmtpError::TooLarge);
        }
        let result = self.transaction(&to, &data).await;
        match result {
            Err(SmtpError::Rejected(_) | SmtpError::Deferred(_)) => {
                // Back to a clean state for the next message.
                if self.wire.exchange(b"RSET\r\n").await.is_err() {
                    self.broken = true;
                }
            }
            Err(_) => self.broken = true,
            Ok(()) => {}
        }
        result
    }

    async fn transaction(&mut self, to: &str, data: &[u8]) -> Result<(), SmtpError> {
        let reply = self
            .wire
            .exchange(format!("MAIL FROM:<{}>\r\n", self.from).as_bytes())
            .await?;
        if reply.code != 250 {
            return Err(refusal(reply.code));
        }
        let reply = self
            .wire
            .exchange(format!("RCPT TO:<{to}>\r\n").as_bytes())
            .await?;
        if reply.code != 250 && reply.code != 251 {
            return Err(refusal(reply.code));
        }
        let reply = self.wire.exchange(b"DATA\r\n").await?;
        if reply.code != 354 {
            return Err(refusal(reply.code));
        }
        let reply = self.wire.exchange(data).await?;
        if reply.code != 250 {
            return Err(refusal(reply.code));
        }
        Ok(())
    }

    /// Ends the session politely (errors are ignored).
    pub async fn quit(mut self) {
        if !self.broken {
            let _ = self.wire.exchange(b"QUIT\r\n").await;
        }
        let _ = tokio::time::timeout(self.wire.timeout, self.wire.stream.shutdown()).await;
    }
}

#[cfg(test)]
mod tests;
