//! Minimal systemd service notifications (`sd_notify(3)`) without libsystemd.
//!
//! Under a unit with `Type=notify`, systemd passes the path of a datagram
//! socket in `NOTIFY_SOCKET`; CalCraft reports `READY=1` once the database is
//! open and the listener is bound, `STOPPING=1` when a graceful shutdown
//! starts, and `WATCHDOG=1` keep-alives when the unit sets `WatchdogSec=`.
//! Outside systemd (no `NOTIFY_SOCKET`, Docker, macOS, Windows) every call is
//! a no-op. Failures are logged at debug level and never stop the server.

use std::time::Duration;

/// Sends `state` (newline-separated `KEY=VALUE` assignments) to the systemd
/// notification socket. Returns whether a message was sent.
pub fn notify(state: &str) -> bool {
    #[cfg(unix)]
    {
        let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
            return false;
        };
        match unix::send(&socket, state) {
            Ok(()) => true,
            Err(err) => {
                tracing::debug!(%err, "systemd notification failed");
                false
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = state;
        false
    }
}

/// The keep-alive interval requested by systemd (`WatchdogSec=`): half the
/// watchdog timeout, as `sd_watchdog_enabled(3)` recommends. `None` when no
/// watchdog is configured for this process.
pub fn watchdog_interval() -> Option<Duration> {
    std::env::var_os("NOTIFY_SOCKET")?;
    parse_watchdog(
        std::env::var("WATCHDOG_USEC").ok().as_deref(),
        std::env::var("WATCHDOG_PID").ok().as_deref(),
        std::process::id(),
    )
}

/// Sends `WATCHDOG=1` at the requested interval from a background task
/// while the async runtime is responsive. Does nothing without a watchdog.
pub fn spawn_watchdog() {
    let Some(period) = watchdog_interval() else {
        return;
    };
    tracing::debug!(?period, "systemd watchdog enabled");
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(period);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            notify("WATCHDOG=1");
        }
    });
}

fn parse_watchdog(usec: Option<&str>, pid: Option<&str>, own_pid: u32) -> Option<Duration> {
    // WATCHDOG_PID names the process the watchdog is meant for; a child that
    // inherited the environment must not answer for it.
    if let Some(pid) = pid
        && pid.trim().parse::<u32>().ok()? != own_pid
    {
        return None;
    }
    let usec = usec?.trim().parse::<u64>().ok().filter(|&usec| usec > 0)?;
    Some(Duration::from_micros(usec / 2).max(Duration::from_millis(1)))
}

#[cfg(unix)]
mod unix {
    use std::{
        ffi::OsStr,
        io,
        os::unix::{ffi::OsStrExt, net::UnixDatagram},
    };

    /// Sends one datagram to `socket`: an absolute path, or `@name` for a
    /// Linux abstract socket.
    pub(super) fn send(socket: &OsStr, state: &str) -> io::Result<()> {
        let bytes = socket.as_bytes();
        let sender = UnixDatagram::unbound()?;
        let sent = match bytes.first() {
            Some(b'/') => sender.send_to(state.as_bytes(), socket)?,
            Some(b'@') => send_abstract(&sender, &bytes[1..], state)?,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "NOTIFY_SOCKET is neither an absolute path nor an abstract socket name",
                ));
            }
        };
        if sent != state.len() {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "systemd notification truncated",
            ));
        }
        Ok(())
    }

    #[cfg(target_os = "linux")]
    fn send_abstract(sender: &UnixDatagram, name: &[u8], state: &str) -> io::Result<usize> {
        use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};
        let addr = SocketAddr::from_abstract_name(name)?;
        sender.send_to_addr(state.as_bytes(), &addr)
    }

    #[cfg(not(target_os = "linux"))]
    fn send_abstract(_sender: &UnixDatagram, _name: &[u8], _state: &str) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "abstract NOTIFY_SOCKET addresses exist only on Linux",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchdog_interval_is_half_the_timeout() {
        assert_eq!(
            parse_watchdog(Some("30000000"), None, 7),
            Some(Duration::from_secs(15))
        );
        assert_eq!(
            parse_watchdog(Some("30000000"), Some("7"), 7),
            Some(Duration::from_secs(15))
        );
    }

    #[test]
    fn watchdog_ignored_when_unset_invalid_or_for_another_process() {
        assert_eq!(parse_watchdog(None, None, 7), None);
        assert_eq!(parse_watchdog(Some("0"), None, 7), None);
        assert_eq!(parse_watchdog(Some("-5"), None, 7), None);
        assert_eq!(parse_watchdog(Some("soon"), None, 7), None);
        assert_eq!(parse_watchdog(Some("30000000"), Some("8"), 7), None);
        assert_eq!(parse_watchdog(Some("30000000"), Some("x"), 7), None);
    }

    #[cfg(unix)]
    mod unix_socket {
        use std::{
            ffi::OsStr,
            os::unix::net::UnixDatagram,
            path::PathBuf,
            sync::atomic::{AtomicU32, Ordering},
            time::Duration,
        };

        use super::super::unix::send;

        /// A short, unique socket path (macOS limits paths to 104 bytes).
        fn socket_path() -> PathBuf {
            static N: AtomicU32 = AtomicU32::new(0);
            let n = N.fetch_add(1, Ordering::Relaxed);
            std::env::temp_dir().join(format!("cc-notify-{}-{n}.sock", std::process::id()))
        }

        fn receive(receiver: &UnixDatagram) -> String {
            receiver
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap_or_else(|err| panic!("set_read_timeout: {err}"));
            let mut buf = [0u8; 512];
            let n = receiver
                .recv(&mut buf)
                .unwrap_or_else(|err| panic!("recv: {err}"));
            String::from_utf8_lossy(&buf[..n]).into_owned()
        }

        #[test]
        fn sends_to_a_path_socket() {
            let path = socket_path();
            let _ = std::fs::remove_file(&path);
            let receiver = UnixDatagram::bind(&path).unwrap_or_else(|err| panic!("bind: {err}"));
            let result = send(path.as_os_str(), "READY=1\nSTATUS=Listening");
            let message = receive(&receiver);
            let _ = std::fs::remove_file(&path);
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(message, "READY=1\nSTATUS=Listening");
        }

        #[test]
        fn rejects_relative_and_empty_addresses() {
            assert!(send(OsStr::new("relative.sock"), "READY=1").is_err());
            assert!(send(OsStr::new(""), "READY=1").is_err());
        }

        #[test]
        fn missing_socket_is_an_error_not_a_panic() {
            let path = socket_path();
            let _ = std::fs::remove_file(&path);
            assert!(send(path.as_os_str(), "STOPPING=1").is_err());
        }

        #[cfg(target_os = "linux")]
        #[test]
        fn sends_to_an_abstract_socket() {
            use std::os::{linux::net::SocketAddrExt, unix::net::SocketAddr};

            let name = format!("calcraft-notify-test-{}", std::process::id());
            let addr = SocketAddr::from_abstract_name(name.as_bytes())
                .unwrap_or_else(|err| panic!("abstract name: {err}"));
            let receiver =
                UnixDatagram::bind_addr(&addr).unwrap_or_else(|err| panic!("bind: {err}"));
            let result = send(OsStr::new(&format!("@{name}")), "WATCHDOG=1");
            assert!(result.is_ok(), "{result:?}");
            assert_eq!(receive(&receiver), "WATCHDOG=1");
        }

        #[cfg(not(target_os = "linux"))]
        #[test]
        fn abstract_socket_is_unsupported_elsewhere() {
            assert!(send(OsStr::new("@calcraft"), "READY=1").is_err());
        }
    }
}
