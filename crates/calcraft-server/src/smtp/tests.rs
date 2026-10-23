//! The SMTP client against an in-process fake server: STARTTLS and
//! implicit TLS with a test CA, AUTH PLAIN/LOGIN, multi-line and malformed
//! replies, dot-stuffing, timeouts, oversized replies, 4xx/5xx.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

use base64::{Engine, engine::general_purpose::STANDARD};
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpListener,
};
use tokio_rustls::TlsAcceptor;

use super::*;
use crate::config::{SmtpConfig, SmtpTls};

/// Test CA (P-256, valid until 2126) and a certificate for `localhost` and
/// 127.0.0.1 signed by it. Test fixtures only.
const CA_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIBhjCCASygAwIBAgIUY6nbb5D7cR7MbPRUbe3p4CHlnCEwCgYIKoZIzj0EAwIw
IDEeMBwGA1UEAwwVQ2FsQ3JhZnQgU01UUCB0ZXN0IENBMCAXDTI2MTAyMzE1NTk0
MFoYDzIxMjYwOTI5MTU1OTQwWjAgMR4wHAYDVQQDDBVDYWxDcmFmdCBTTVRQIHRl
c3QgQ0EwWTATBgcqhkjOPQIBBggqhkjOPQMBBwNCAAQiAKobZlFX803MTORFXQyO
hDWBT3+2dSnUntJTyjdKF28o2aU761v5e221MGamPPsHJA9h5GiqsbtT2ZJPWQQy
o0IwQDAPBgNVHRMBAf8EBTADAQH/MA4GA1UdDwEB/wQEAwIBBjAdBgNVHQ4EFgQU
lOzJf2joCsAETLDeChsNcauzW0owCgYIKoZIzj0EAwIDSAAwRQIhAKD49cvGUyrr
3zNpi1+G4vOEQcrZatObwb0+SSmy/nAtAiBjORzauxS4p6dQ7rUwPmlXw302FGr/
Tp80SnpI67IddQ==
-----END CERTIFICATE-----
";

const LEAF_PEM: &str = "-----BEGIN CERTIFICATE-----
MIIByjCCAXGgAwIBAgIULQZgkvv3Xons0LCdF4XGo16nVE4wCgYIKoZIzj0EAwIw
IDEeMBwGA1UEAwwVQ2FsQ3JhZnQgU01UUCB0ZXN0IENBMCAXDTI2MTAyMzE1NTk0
MFoYDzIxMjYwOTI5MTU1OTQwWjAUMRIwEAYDVQQDDAlsb2NhbGhvc3QwWTATBgcq
hkjOPQIBBggqhkjOPQMBBwNCAASAaEYNlg4FAog68n6BOjdZ4HSLSW49ub1J+7Q1
hZD7O3nI3tgrd+4EnVy+zG8qldMU1YVjOK9vVUF/KE0Q7uswo4GSMIGPMAwGA1Ud
EwEB/wQCMAAwDgYDVR0PAQH/BAQDAgeAMBMGA1UdJQQMMAoGCCsGAQUFBwMBMBoG
A1UdEQQTMBGCCWxvY2FsaG9zdIcEfwAAATAfBgNVHSMEGDAWgBSU7Ml/aOgKwARM
sN4KGw1xq7NbSjAdBgNVHQ4EFgQUDOGUxNEfxwKD2OvLl8jO0yWVJHwwCgYIKoZI
zj0EAwIDRwAwRAIgL2nraTQGYiqR1Koo1ejSFLojEr1uTmLfI1y3E8Qw5r0CIHSc
5uAhO1WTYpgIXXEK6TE6azHzzPL6aO4iFozp2k0d
-----END CERTIFICATE-----
";

const LEAF_KEY: &str = "-----BEGIN PRIVATE KEY-----
MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgN+3SntCvbYNF/htY
Mm20aSvdXdKK3mvNaE6Jk0rGO2ihRANCAASAaEYNlg4FAog68n6BOjdZ4HSLSW49
ub1J+7Q1hZD7O3nI3tgrd+4EnVy+zG8qldMU1YVjOK9vVUF/KE0Q7usw
-----END PRIVATE KEY-----
";

const USER: &str = "calcraft-test";
const PASSWORD: &str = "fake smtp pass";

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "calcraft-smtp-{}",
            crate::secrets::random_token::<8>()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn write(&self, name: &str, content: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, content).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How the fake server behaves.
#[derive(Clone)]
struct Fake {
    tls: SmtpTls,
    /// Offer STARTTLS in starttls mode.
    offer_starttls: bool,
    /// Mechanisms offered after TLS; empty: no AUTH line.
    mechanisms: &'static str,
    greeting: &'static str,
    /// Sent together with the 220 to STARTTLS (command injection).
    inject_after_starttls: bool,
    /// Never answer anything.
    silent: bool,
    /// EHLO reply with this many extra lines.
    ehlo_padding: usize,
    mail_code: u16,
    rcpt_code: u16,
    data_code: u16,
    size: Option<u64>,
}

impl Fake {
    fn new(tls: SmtpTls) -> Self {
        Self {
            tls,
            offer_starttls: true,
            mechanisms: "PLAIN LOGIN",
            greeting: "220-fake.example.org ESMTP\r\n220 ready\r\n",
            inject_after_starttls: false,
            silent: false,
            ehlo_padding: 0,
            mail_code: 250,
            rcpt_code: 250,
            data_code: 250,
            size: None,
        }
    }
}

/// What the fake server saw.
#[derive(Debug, Default)]
struct Seen {
    /// Commands with whether they arrived over TLS (AUTH lines redacted).
    commands: Vec<(bool, String)>,
    /// Successful AUTH (mechanism, over TLS).
    auth: Option<(String, bool)>,
    recipients: Vec<String>,
    /// Messages after removing the dot-stuffing.
    messages: Vec<Vec<u8>>,
}

fn acceptor() -> TlsAcceptor {
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_slice_iter(LEAF_PEM.as_bytes())
        .collect::<Result<_, _>>()
        .unwrap();
    let key = PrivateKeyDer::from_pem_slice(LEAF_KEY.as_bytes()).unwrap();
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .unwrap();
    TlsAcceptor::from(Arc::new(config))
}

async fn start(fake: Fake) -> (u16, Arc<Mutex<Seen>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let shared = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else {
                return;
            };
            let fake = fake.clone();
            let seen = shared.clone();
            tokio::spawn(async move {
                if fake.silent {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    drop(tcp);
                    return;
                }
                if fake.tls == SmtpTls::Implicit {
                    let Ok(tls) = acceptor().accept(tcp).await else {
                        return;
                    };
                    let mut stream = BufReader::new(tls);
                    let _ = stream.get_mut().write_all(fake.greeting.as_bytes()).await;
                    let _ = serve(stream, &fake, &seen, true).await;
                    return;
                }
                let mut stream = BufReader::new(tcp);
                let _ = stream.get_mut().write_all(fake.greeting.as_bytes()).await;
                if let Some(tcp) = serve(stream, &fake, &seen, false).await
                    && let Ok(tls) = acceptor().accept(tcp).await
                {
                    let _ = serve(BufReader::new(tls), &fake, &seen, true).await;
                }
            });
        }
    });
    (port, seen)
}

async fn read_line<S: AsyncRead + Unpin>(stream: &mut BufReader<S>) -> Option<String> {
    let mut line = String::new();
    match stream.read_line(&mut line).await {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

/// Serves commands; returns the stream when the client asked for STARTTLS.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: BufReader<S>,
    fake: &Fake,
    seen: &Mutex<Seen>,
    tls: bool,
) -> Option<S> {
    loop {
        let line = read_line(&mut stream).await?;
        let command = line.trim_end().to_owned();
        let upper = command.to_ascii_uppercase();
        let logged = if upper.starts_with("AUTH") {
            "AUTH …".to_owned()
        } else {
            command.clone()
        };
        seen.lock().unwrap().commands.push((tls, logged));
        let reply: String = if upper.starts_with("EHLO") {
            let mut reply = String::from("250-fake.example.org\r\n");
            for n in 0..fake.ehlo_padding {
                reply.push_str(&format!("250-X-PAD{n} {}\r\n", "p".repeat(200)));
            }
            if fake.tls == SmtpTls::Starttls && fake.offer_starttls && !tls {
                reply.push_str("250-STARTTLS\r\n");
            }
            if tls && !fake.mechanisms.is_empty() {
                reply.push_str(&format!("250-AUTH {}\r\n", fake.mechanisms));
            }
            if let Some(size) = fake.size {
                reply.push_str(&format!("250-SIZE {size}\r\n"));
            }
            reply.push_str("250 8BITMIME\r\n");
            reply
        } else if upper == "STARTTLS" {
            let mut reply = String::from("220 go ahead\r\n");
            if fake.inject_after_starttls {
                reply.push_str("250 injected\r\n");
            }
            stream.get_mut().write_all(reply.as_bytes()).await.ok()?;
            return Some(stream.into_inner());
        } else if let Some(credentials) = command.strip_prefix("AUTH PLAIN ") {
            let decoded = STANDARD.decode(credentials).unwrap_or_default();
            let expected = format!("\0{USER}\0{PASSWORD}");
            if decoded == expected.as_bytes() {
                seen.lock().unwrap().auth = Some(("PLAIN".to_owned(), tls));
                "235 ok\r\n".to_owned()
            } else {
                "535 bad credentials\r\n".to_owned()
            }
        } else if upper == "AUTH LOGIN" {
            stream
                .get_mut()
                .write_all(b"334 VXNlcm5hbWU6\r\n")
                .await
                .ok()?;
            let user = read_line(&mut stream).await?;
            stream
                .get_mut()
                .write_all(b"334 UGFzc3dvcmQ6\r\n")
                .await
                .ok()?;
            let password = read_line(&mut stream).await?;
            let user = STANDARD.decode(user.trim_end()).unwrap_or_default();
            let password = STANDARD.decode(password.trim_end()).unwrap_or_default();
            if user == USER.as_bytes() && password == PASSWORD.as_bytes() {
                seen.lock().unwrap().auth = Some(("LOGIN".to_owned(), tls));
                "235 ok\r\n".to_owned()
            } else {
                "535 bad credentials\r\n".to_owned()
            }
        } else if upper.starts_with("MAIL FROM:") {
            format!("{} mail\r\n", fake.mail_code)
        } else if let Some(to) = command.strip_prefix("RCPT TO:") {
            seen.lock().unwrap().recipients.push(to.to_owned());
            format!("{} rcpt\r\n", fake.rcpt_code)
        } else if upper == "DATA" {
            stream.get_mut().write_all(b"354 go\r\n").await.ok()?;
            let mut message = Vec::new();
            loop {
                let line = read_line(&mut stream).await?;
                if line == ".\r\n" {
                    break;
                }
                let line = line.strip_prefix('.').unwrap_or(&line);
                message.extend_from_slice(line.as_bytes());
            }
            seen.lock().unwrap().messages.push(message);
            format!("{} queued\r\n", fake.data_code)
        } else if upper == "RSET" {
            "250 reset\r\n".to_owned()
        } else if upper == "QUIT" {
            let _ = stream.get_mut().write_all(b"221 bye\r\n").await;
            return None;
        } else {
            "500 unknown\r\n".to_owned()
        };
        stream.get_mut().write_all(reply.as_bytes()).await.ok()?;
    }
}

struct Setup {
    client: SmtpClient,
    seen: Arc<Mutex<Seen>>,
    _dir: TempDir,
}

async fn prepare(fake: Fake, auth: bool, trust: bool) -> Setup {
    let tls = fake.tls;
    let (port, seen) = start(fake).await;
    let dir = TempDir::new();
    let mut config = SmtpConfig {
        enabled: true,
        host: "127.0.0.1".to_owned(),
        port: Some(port),
        tls,
        from: "calendar@example.org".to_owned(),
        ..SmtpConfig::default()
    };
    if auth {
        config.username = Some(USER.to_owned());
        config.password_file = Some(dir.write("password", &format!("{PASSWORD}\n")));
    }
    if trust {
        config.ca_file = Some(dir.write("ca.pem", CA_PEM));
    }
    let client = SmtpClient::new(&config)
        .unwrap()
        .with_step_timeout(Duration::from_secs(2));
    Setup {
        client,
        seen,
        _dir: dir,
    }
}

fn message(body: &str) -> Vec<u8> {
    calcraft_core::mail::Message {
        from_name: "CalCraft",
        from: "calendar@example.org",
        to: "alice@example.org",
        subject: "Reminder: test",
        body,
        date: 1_800_000_000,
        id: "test1",
    }
    .render()
    .unwrap()
}

#[tokio::test]
async fn starttls_with_auth_plain_delivers_the_message() {
    let setup = prepare(Fake::new(SmtpTls::Starttls), true, true).await;
    let mut connection = setup.client.connect().await.unwrap();
    let sent = message("first\n.\n..two dots\nlast");
    connection.send("alice@example.org", &sent).await.unwrap();
    connection.quit().await;
    let seen = setup.seen.lock().unwrap();
    assert_eq!(seen.auth, Some(("PLAIN".to_owned(), true)));
    assert_eq!(seen.recipients, vec!["<alice@example.org>"]);
    // Dot-stuffing round trip: the server got exactly the message.
    assert_eq!(seen.messages, vec![sent]);
    // Nothing but EHLO and STARTTLS before TLS.
    let plain: Vec<&str> = seen
        .commands
        .iter()
        .filter(|(tls, _)| !tls)
        .map(|(_, c)| c.as_str())
        .collect();
    assert_eq!(plain, vec!["EHLO example.org", "STARTTLS"]);
    assert!(
        seen.commands
            .iter()
            .any(|(tls, c)| *tls && c == "MAIL FROM:<calendar@example.org>")
    );
}

#[tokio::test]
async fn implicit_tls_with_auth_login() {
    let mut fake = Fake::new(SmtpTls::Implicit);
    fake.mechanisms = "LOGIN";
    let setup = prepare(fake, true, true).await;
    let mut connection = setup.client.connect().await.unwrap();
    connection
        .send("alice@example.org", &message("hello"))
        .await
        .unwrap();
    connection.quit().await;
    let seen = setup.seen.lock().unwrap();
    assert_eq!(seen.auth, Some(("LOGIN".to_owned(), true)));
    assert_eq!(seen.messages.len(), 1);
    assert!(seen.commands.iter().all(|(tls, _)| *tls));
}

#[tokio::test]
async fn starttls_is_required() {
    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.offer_starttls = false;
    let setup = prepare(fake, true, true).await;
    assert_eq!(
        setup.client.connect().await.err(),
        Some(SmtpError::NoStartTls)
    );
    // No credentials were sent.
    let seen = setup.seen.lock().unwrap();
    assert!(seen.commands.iter().all(|(_, c)| !c.starts_with("AUTH")));
}

#[tokio::test]
async fn data_injected_before_the_tls_handshake_is_refused() {
    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.inject_after_starttls = true;
    let setup = prepare(fake, true, true).await;
    assert_eq!(
        setup.client.connect().await.err(),
        Some(SmtpError::Protocol)
    );
}

#[tokio::test]
async fn untrusted_certificates_are_refused() {
    for tls in [SmtpTls::Starttls, SmtpTls::Implicit] {
        let setup = prepare(Fake::new(tls), true, false).await;
        assert_eq!(setup.client.connect().await.err(), Some(SmtpError::Tls));
        let seen = setup.seen.lock().unwrap();
        assert!(seen.auth.is_none());
    }
}

#[tokio::test]
async fn no_supported_mechanism() {
    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.mechanisms = "CRAM-MD5 XOAUTH2";
    let setup = prepare(fake, true, true).await;
    assert_eq!(setup.client.connect().await.err(), Some(SmtpError::NoAuth));
}

#[tokio::test]
async fn wrong_password() {
    let (port, _) = start(Fake::new(SmtpTls::Starttls)).await;
    let dir = TempDir::new();
    let config = SmtpConfig {
        enabled: true,
        host: "127.0.0.1".to_owned(),
        port: Some(port),
        username: Some(USER.to_owned()),
        password_file: Some(dir.write("password", "wrong\n")),
        ca_file: Some(dir.write("ca.pem", CA_PEM)),
        from: "calendar@example.org".to_owned(),
        ..SmtpConfig::default()
    };
    let client = SmtpClient::new(&config).unwrap();
    let err = client.connect().await.err().unwrap();
    assert_eq!(err, SmtpError::Auth(535));
    assert!(err.transient());
    assert_eq!(err.code(), "smtp_auth");
    // The client's debug output names no credentials.
    let debug = format!("{client:?}");
    assert!(!debug.contains("wrong") && !debug.contains(USER), "{debug}");
}

#[tokio::test]
async fn plain_relay_on_loopback_without_auth() {
    let setup = prepare(Fake::new(SmtpTls::None), false, false).await;
    let mut connection = setup.client.connect().await.unwrap();
    connection
        .send("alice@example.org", &message("hi"))
        .await
        .unwrap();
    connection.quit().await;
    assert_eq!(setup.seen.lock().unwrap().messages.len(), 1);
}

#[tokio::test]
async fn temporary_and_permanent_refusals() {
    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.rcpt_code = 451;
    let setup = prepare(fake, true, true).await;
    let mut connection = setup.client.connect().await.unwrap();
    let err = connection
        .send("alice@example.org", &message("x"))
        .await
        .unwrap_err();
    assert_eq!(err, SmtpError::Deferred(451));
    assert!(err.transient());
    // RSET keeps the session usable.
    assert!(connection.usable());
    assert!(
        setup
            .seen
            .lock()
            .unwrap()
            .commands
            .iter()
            .any(|(_, c)| c == "RSET")
    );

    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.data_code = 554;
    let setup = prepare(fake, true, true).await;
    let mut connection = setup.client.connect().await.unwrap();
    let err = connection
        .send("alice@example.org", &message("x"))
        .await
        .unwrap_err();
    assert_eq!(err, SmtpError::Rejected(554));
    assert!(!err.transient());
    assert_eq!(err.code(), "smtp_rejected");

    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.greeting = "554 go away\r\n";
    let setup = prepare(fake, true, true).await;
    assert_eq!(
        setup.client.connect().await.err(),
        Some(SmtpError::Rejected(554))
    );
}

#[tokio::test]
async fn timeouts() {
    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.silent = true;
    let setup = prepare(fake, true, true).await;
    let started = std::time::Instant::now();
    assert_eq!(setup.client.connect().await.err(), Some(SmtpError::Timeout));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn oversized_and_malformed_replies() {
    // Many long EHLO lines: more than the reply limit.
    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.ehlo_padding = 100;
    let setup = prepare(fake, true, true).await;
    assert_eq!(
        setup.client.connect().await.err(),
        Some(SmtpError::Protocol)
    );

    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.greeting = "hello there\r\n";
    let setup = prepare(fake, true, true).await;
    assert_eq!(
        setup.client.connect().await.err(),
        Some(SmtpError::Protocol)
    );

    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.greeting = Box::leak(format!("220 {}\r\n", "x".repeat(5000)).into_boxed_str());
    let setup = prepare(fake, true, true).await;
    assert_eq!(
        setup.client.connect().await.err(),
        Some(SmtpError::Protocol)
    );
}

#[tokio::test]
async fn size_limit_and_invalid_input() {
    let mut fake = Fake::new(SmtpTls::Starttls);
    fake.size = Some(100);
    let setup = prepare(fake, true, true).await;
    let mut connection = setup.client.connect().await.unwrap();
    assert_eq!(
        connection
            .send("alice@example.org", &message(&"x".repeat(500)))
            .await,
        Err(SmtpError::TooLarge)
    );
    // Addresses that would inject commands, and messages with bare line
    // ends, never reach the server.
    assert_eq!(
        connection
            .send(
                "alice@example.org>\r\nRCPT TO:<evil@example.org",
                &message("x")
            )
            .await,
        Err(SmtpError::Message)
    );
    assert_eq!(
        connection
            .send("alice@example.org", b"Subject: x\r\n\r\nbare\nlf\r\n")
            .await,
        Err(SmtpError::Message)
    );
    assert!(setup.seen.lock().unwrap().recipients.is_empty());
}

#[tokio::test]
async fn unreachable_server() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let config = SmtpConfig {
        enabled: true,
        host: "127.0.0.1".to_owned(),
        port: Some(port),
        tls: SmtpTls::None,
        from: "calendar@example.org".to_owned(),
        ..SmtpConfig::default()
    };
    let client = SmtpClient::new(&config).unwrap();
    let err = client.connect().await.err().unwrap();
    assert_eq!(err, SmtpError::Connect);
    assert!(err.transient());
}

#[test]
fn password_file_permissions_are_checked() {
    let dir = TempDir::new();
    let path = dir.write("password", "secret\n");
    let config = SmtpConfig {
        enabled: true,
        host: "smtp.example.org".to_owned(),
        username: Some(USER.to_owned()),
        password_file: Some(path.clone()),
        from: "calendar@example.org".to_owned(),
        ..SmtpConfig::default()
    };
    SmtpClient::new(&config).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let err = SmtpClient::new(&config).unwrap_err().to_string();
        assert!(err.contains("chmod 600"), "{err}");
        assert!(!err.contains("secret"));
    }
    let missing = SmtpConfig {
        password_file: Some(dir.0.join("missing")),
        ..config
    };
    assert!(SmtpClient::new(&missing).is_err());
}
