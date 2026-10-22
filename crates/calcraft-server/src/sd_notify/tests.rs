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
        let receiver = UnixDatagram::bind_addr(&addr).unwrap_or_else(|err| panic!("bind: {err}"));
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
