use super::*;

fn from_toml(toml: &str) -> Result<Config, ConfigError> {
    Config::from_figment(
        Figment::from(Serialized::defaults(Config::default())).merge(Toml::string(toml)),
    )
}

#[test]
fn defaults_are_secure() {
    let config = Config::default();
    assert!(config.server.bind.ip().is_loopback());
    assert!(!config.security.hsts);
    config.validate().unwrap();
}

#[test]
fn partial_file_keeps_defaults() {
    let config = from_toml("[web_ui]\nenabled = false\n").unwrap();
    assert!(!config.web_ui.enabled);
    assert!(config.api.enabled);
    assert_eq!(config.server, ServerConfig::default());
}

#[test]
fn unknown_keys_are_rejected() {
    assert!(from_toml("[security]\nhts = true\n").is_err());
    assert!(from_toml("[unknown]\nkey = 1\n").is_err());
}

#[test]
fn zero_limits_are_rejected() {
    assert!(from_toml("[server]\nmax_body_bytes = 0\n").is_err());
    assert!(from_toml("[server]\nrequest_timeout_secs = 0\n").is_err());
    assert!(from_toml("[storage]\npre_migration_backups_keep = 0\n").is_err());
}

#[test]
fn request_limits_are_on_by_default_and_validated() {
    let server = Config::default().server;
    assert_eq!(server.max_concurrent_requests, 256);
    assert_eq!(server.max_concurrent_requests_per_client, 32);
    assert_eq!(server.client_rate_burst, 120);
    assert_eq!(server.client_rate_per_second, 20);
    assert_eq!(server.max_login_failures_per_client, 20);
    assert_eq!(server.client_ipv6_prefix, 64);
    // A reverse proxy on the same host is trusted by default.
    assert_eq!(server.trusted_proxies, Cidr::loopback());
    // Trusting nobody has to be configured explicitly.
    let config = from_toml("[server]\ntrusted_proxies = []\n").unwrap();
    assert!(config.server.trusted_proxies.is_empty());

    let config =
        from_toml("[server]\ntrusted_proxies = [\"127.0.0.1/32\", \"::1\", \"10.0.0.0/8\"]\n")
            .unwrap();
    let proxies: Vec<String> = config
        .server
        .trusted_proxies
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(proxies, ["127.0.0.1/32", "::1/128", "10.0.0.0/8"]);

    for bad in [
        "max_concurrent_requests = 0",
        "max_concurrent_requests_per_client = 0",
        "client_rate_burst = 0",
        "client_rate_per_second = 0",
        "max_login_failures_per_client = 0",
        "max_tracked_clients = 0",
        "max_concurrent_requests = 2000000",
        "max_tracked_clients = 255",
        "max_tracked_clients = 511",
        "max_tracked_clients = 100001",
        "client_ipv6_prefix = 31",
        "client_ipv6_prefix = 129",
        "trusted_proxies = [\"10.0.0.1/8\"]",
        "trusted_proxies = [\"proxy.example\"]",
        "trusted_proxies = \"127.0.0.1\"",
    ] {
        assert!(from_toml(&format!("[server]\n{bad}\n")).is_err(), "{bad}");
    }
}

#[test]
fn pre_migration_backup_is_on_by_default() {
    let config = Config::default();
    assert!(config.storage.pre_migration_backup);
    assert_eq!(config.storage.pre_migration_backups_keep, 3);
    let config =
        from_toml("[storage]\npre_migration_backup = false\npre_migration_backups_keep = 5\n")
            .unwrap();
    assert!(!config.storage.store_options().pre_migration_backup);
    assert_eq!(config.storage.store_options().pre_migration_backups_keep, 5);
}

#[test]
fn trash_retention() {
    let config = Config::default();
    assert_eq!(config.storage.trash_retention_days, 30);
    assert_eq!(config.storage.store_options().trash_retention_days, 30);
    let config = from_toml("[storage]\ntrash_retention_days = 0\n").unwrap();
    assert_eq!(config.storage.store_options().trash_retention_days, 0);
    assert!(from_toml("[storage]\ntrash_retention_days = 3651\n").is_err());
    assert!(from_toml("[storage]\ntrash_retention_days = -1\n").is_err());
}

#[test]
fn vcard_limit_is_bounded() {
    assert_eq!(Config::default().max_vcard_bytes(), DEFAULT_MAX_VCARD_BYTES);
    let config = from_toml("[storage]\nmax_vcard_bytes = 10485760\n").unwrap();
    assert_eq!(config.max_vcard_bytes(), MAX_VCARD_BYTES);
    for toml in [
        "[storage]\nmax_vcard_bytes = 10485761\n",
        "[storage]\nmax_vcard_bytes = 0\n",
        "[storage]\nmax_vcard_bytes = 65535\n",
        // An explicit value above the body limit is a mistake.
        "[server]\nmax_body_bytes = 1048576\n[storage]\nmax_vcard_bytes = 2097152\n",
    ] {
        assert!(from_toml(toml).is_err(), "{toml}");
    }
    let config =
        from_toml("[server]\nmax_body_bytes = 1048576\n[storage]\nmax_vcard_bytes = 1048576\n")
            .unwrap();
    assert_eq!(config.max_vcard_bytes(), 1024 * 1024);
}

#[test]
fn vcard_default_follows_a_smaller_body_limit() {
    // Configurations from before the setting existed keep working.
    let config = from_toml("[server]\nmax_body_bytes = 1048576\n").unwrap();
    assert_eq!(config.storage.max_vcard_bytes, None);
    assert_eq!(config.max_vcard_bytes(), 1024 * 1024);
    let config = from_toml("[server]\nmax_body_bytes = 20971520\n").unwrap();
    assert_eq!(config.max_vcard_bytes(), DEFAULT_MAX_VCARD_BYTES);
}

const OIDC: &str = r#"[oidc]
enabled = true
issuer = "https://idp.example/realms/main"
client_id = "calcraft"
client_secret = "s3cret"
redirect_uri = "https://cal.example/api/v1/auth/oidc/callback"
"#;

#[test]
fn oidc_is_off_by_default_and_validated() {
    assert!(!Config::default().oidc.enabled);
    let config = from_toml(OIDC).unwrap();
    assert_eq!(config.oidc.username_claim, "preferred_username");
    assert!(!config.oidc.auto_create_users);
    // The secret never appears in debug output (check-config).
    assert!(!format!("{config:?}").contains("s3cret"));

    for (from, to) in [
        (
            "https://idp.example/realms/main",
            "http://idp.example/realms/main",
        ),
        (
            "https://idp.example/realms/main",
            "https://idp.example/r?x=1",
        ),
        (
            "https://idp.example/realms/main",
            "https://user@idp.example/r",
        ),
        ("https://idp.example/realms/main", "ftp://idp.example/r"),
        (
            "https://cal.example/api/v1/auth/oidc/callback",
            "http://cal.example/api/v1/auth/oidc/callback",
        ),
        (
            "https://cal.example/api/v1/auth/oidc/callback",
            "https://cal.example/other",
        ),
        (
            "client_secret = \"s3cret\"",
            "client_secret_file = \"/x\"\nclient_secret = \"s\"",
        ),
        (
            "client_secret = \"s3cret\"",
            "token_auth_method = \"client_secret_post\"",
        ),
    ] {
        assert!(from_toml(&OIDC.replace(from, to)).is_err(), "{to}");
    }
    for extra in [
        "signing_algs = [\"none\"]",
        "signing_algs = [\"HS256\"]",
        "signing_algs = []",
        "clock_skew_secs = 61",
        "scopes = [\"profile\"]",
        "scopes = [\"openid\", \"a b\"]",
        "admin_claim = \"groups\"",
        "token_auth_method = \"none\"",
    ] {
        assert!(from_toml(&format!("{OIDC}{extra}\n")).is_err(), "{extra}");
    }
    // http only with the explicit testing flag.
    let insecure = OIDC
        .replace("https://idp.example", "http://127.0.0.1:9")
        .replace("https://cal.example", "http://localhost");
    assert!(from_toml(&insecure).is_err());
    from_toml(&format!(
        "{insecure}allow_insecure_issuer_for_testing = true\n"
    ))
    .unwrap();

    // SSO-only mode needs SSO.
    assert!(from_toml("[security]\npassword_login = false\n").is_err());
    let sso_only = from_toml(&format!("{OIDC}[security]\npassword_login = false\n")).unwrap();
    assert!(!sso_only.security.password_login);
    assert!(!sso_only.oidc.require_local_totp);
}

#[test]
fn update_check_is_off_by_default_and_bounded() {
    let config = Config::default();
    assert!(!config.updates.check);
    assert_eq!(config.updates.interval(), Duration::from_secs(24 * 3600));
    let config = from_toml("[updates]\ncheck = true\ninterval_hours = 168\n").unwrap();
    assert!(config.updates.check);
    for bad in [
        "interval_hours = 0",
        "interval_hours = 169",
        "url = \"https://x\"",
    ] {
        assert!(from_toml(&format!("[updates]\n{bad}\n")).is_err(), "{bad}");
    }
}

#[test]
fn subscriptions_are_off_by_default_and_bounded() {
    let config = Config::default();
    let subs = &config.subscriptions;
    assert!(!subs.enabled);
    assert!(!subs.allow_http);
    assert_eq!(subs.ports(), vec![443]);
    assert_eq!(subs.max_bytes, 10 * 1024 * 1024);
    assert_eq!(subs.max_per_user, 20);
    assert_eq!(subs.default_refresh_secs(), 6 * 3600);
    assert_eq!(subs.max_concurrent_fetches, 2);
    assert_eq!(subs.max_redirects, 3);
    let config = from_toml(
        "[subscriptions]\nenabled = true\nallow_http = true\nallowed_hosts = [\"*.example.com\"]\n\
         denied_hosts = [\"bad.example.com\"]\n",
    )
    .unwrap();
    assert!(config.subscriptions.enabled);
    assert_eq!(config.subscriptions.ports(), vec![443, 80]);
    for bad in [
        "max_bytes = 1000",
        "max_bytes = 20971521",
        "max_per_user = 0",
        "max_per_user = 101",
        "default_refresh_minutes = 14",
        "default_refresh_minutes = 10081",
        "max_concurrent_fetches = 0",
        "max_concurrent_fetches = 17",
        "timeout_secs = 4",
        "timeout_secs = 121",
        "max_redirects = 6",
        "allowed_ports = [0]",
        "allowed_hosts = [\"Example.com\"]",
        "allowed_hosts = [\"https://example.com\"]",
        "denied_hosts = [\"exa mple.com\"]",
        "denied_hosts = [\"\"]",
        "allowed_networks = [\"10.0.0.0/8\"]",
    ] {
        assert!(
            from_toml(&format!("[subscriptions]\n{bad}\n")).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn missing_file_is_an_error() {
    let result = Config::load(Some(Path::new("does-not-exist.toml")));
    assert!(matches!(result, Err(ConfigError::FileNotFound(_))));
}

#[test]
fn scheduling_is_off_by_default_and_validated() {
    let scheduling = Config::default().scheduling;
    assert!(!scheduling.enabled);
    assert!(scheduling.free_busy);
    assert_eq!(scheduling.domain, None);
    let config = from_toml("[scheduling]\nenabled = true\ndomain = \"example.org\"\n").unwrap();
    assert!(config.scheduling.enabled);
    for bad in [
        "domain = \"Example.org\"",
        "domain = \"*.example.org\"",
        "domain = \"\"",
        "max_attendees = 0",
        "max_attendees = 1001",
        "max_recipients_per_hour = 0",
        "inbox_max_messages = 5",
        "max_freebusy_days = 0",
        "max_freebusy_days = 4000",
        "enable = true",
    ] {
        assert!(
            from_toml(&format!("[scheduling]\n{bad}\n")).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn attachments_are_off_by_default_and_validated() {
    let config = from_toml("").unwrap();
    assert!(!config.attachments.enabled);
    assert_eq!(config.attachments.max_bytes, 10 * 1024 * 1024);
    let config = from_toml(
        "[attachments]\nenabled = true\nbase_url = \"https://cal.example.org\"\nmax_bytes = 2048\n",
    )
    .unwrap();
    assert_eq!(config.attachments.base(), "https://cal.example.org");
    for bad in [
        "enabled = true",
        "enabled = true\nbase_url = \"https://cal.example.org/sub\"",
        "base_url = \"javascript:alert(1)\"",
        "max_bytes = 1",
        "max_bytes = 104857601",
        "max_per_object = 101",
        "max_bytes_per_user = 100",
        "enable = true",
    ] {
        assert!(
            from_toml(&format!("[attachments]\n{bad}\n")).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn example_configuration_is_valid() {
    let text = include_str!("../../../../calcraft.example.toml");
    from_toml(text).unwrap();
}

#[test]
fn sharing_is_on_and_publishing_off_by_default() {
    let config = from_toml("").unwrap();
    assert!(config.sharing.enabled);
    assert!(!config.publish.enabled);
    assert_eq!(config.publish.base_url, None);
    let config = from_toml(
        "[sharing]\nenabled = false\n[publish]\nenabled = true\nbase_url = \"https://cal.example.org/\"\nmax_objects = 10\n",
    )
    .unwrap();
    assert!(!config.sharing.enabled);
    assert_eq!(config.publish.base(), "https://cal.example.org");
    assert_eq!(config.publish.max_objects, 10);
    for bad in [
        "[publish]\nenabled = true",
        "[publish]\nbase_url = \"https://cal.example.org/public\"",
        "[publish]\nbase_url = \"http://user@cal.example.org\"",
        "[publish]\nmax_bytes = 1",
        "[publish]\nmax_objects = 0",
        "[publish]\nmax_objects = 100001",
        "[publish]\ntoken = \"x\"",
        "[sharing]\nauto_accept = true",
    ] {
        assert!(from_toml(&format!("{bad}\n")).is_err(), "{bad}");
    }
}

#[test]
fn group_limits_have_defaults_and_bounds() {
    let config = from_toml("").unwrap();
    assert_eq!(config.groups.max_groups, 100);
    assert_eq!(config.groups.max_members, 200);
    let config = from_toml("[groups]\nmax_groups = 1\nmax_members = 1000\n").unwrap();
    assert_eq!(
        (config.groups.max_groups, config.groups.max_members),
        (1, 1000)
    );
    for bad in [
        "[groups]\nmax_groups = 0",
        "[groups]\nmax_groups = 10001",
        "[groups]\nmax_members = 0",
        "[groups]\nmax_members = 1001",
        "[groups]\nenabled = true",
    ] {
        assert!(from_toml(&format!("{bad}\n")).is_err(), "{bad}");
    }
}
