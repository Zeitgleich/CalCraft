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
                // A failing watchdog ping gets the service restarted, so the
                // first failure is worth a warning; repeats stay at debug.
                static WARNED: std::sync::atomic::AtomicBool =
                    std::sync::atomic::AtomicBool::new(false);
                if WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                    tracing::debug!(%err, "systemd notification failed");
                } else {
                    tracing::warn!(%err, "systemd notification failed");
                }
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
        // Called from async tasks: never block a runtime worker for long if
        // systemd's socket buffer is full.
        sender.set_write_timeout(Some(std::time::Duration::from_millis(100)))?;
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
mod tests;
