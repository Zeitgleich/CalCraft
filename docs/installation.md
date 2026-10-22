# Installing CalCraft

This guide sets up CalCraft on a Linux server behind a TLS-terminating
reverse proxy (nginx or Apache), reachable as `https://cal.example.com`.
Replace `cal.example.com` and `example.com` with your own names throughout.

> CalCraft is in early development. Back up your data and test before relying on it.

**Contents**

1. [Overview](#1-overview)
2. [Requirements](#2-requirements)
3. [DNS records](#3-dns-records)
4. [Install CalCraft](#4-install-calcraft) – from source or with Docker
5. [Configure](#5-configure)
6. [Run as a systemd service](#6-run-as-a-systemd-service)
7. [Create the first user](#7-create-the-first-user)
8. [TLS certificate](#8-tls-certificate)
9. [Reverse proxy: nginx](#9-reverse-proxy-nginx)
10. [Reverse proxy: Apache](#10-reverse-proxy-apache)
11. [Firewall](#11-firewall)
12. [Connect clients](#12-connect-clients)
13. [Backups](#13-backups)
14. [Updates](#14-updates)
15. [Troubleshooting](#15-troubleshooting)

## 1. Overview

```
 Client (Thunderbird, iOS, DAVx⁵, browser)
            │  HTTPS :443
            ▼
 nginx / Apache  ── TLS, HTTP → HTTPS redirect
            │  HTTP 127.0.0.1:5232
            ▼
 calcraft serve  ── DAV (/dav/), web UI (/), REST API (/api/)
            │
            ▼
 SQLite database (/var/lib/calcraft/calcraft.db)
```

- CalCraft itself speaks plain HTTP and listens on `127.0.0.1` only. TLS is
  terminated by the reverse proxy.
- DAV clients authenticate with HTTP Basic, so **never expose CalCraft without
  TLS**.
- CalCraft must be served at the **root of its own host name**
  (`https://cal.example.com/`), not under a sub-path like
  `https://example.com/calendar/`.

## 2. Requirements

- A Linux server (x86_64 or arm64) with a public IP address. CalCraft also
  runs on macOS and Windows; this guide covers Linux.
- A domain whose DNS you can edit.
- Ports 80 and 443 reachable from the internet (80 is needed for the
  Let's Encrypt HTTP challenge and the redirect to HTTPS).
- One of:
  - **From source:** Rust 1.99 (installed via [rustup](https://rustup.rs)),
    `git`, a C compiler (`build-essential` / `gcc`) for the bundled SQLite.
  - **Docker:** Docker Engine 24 or newer.
- nginx (1.25.1+ for the `http2 on;` syntax used below; see the note in
  section 9 for older versions) or Apache (2.4+), and certbot.

## 3. DNS records

Create these records at your DNS provider (TTL e.g. 3600):

| Name | Type | Value | Required |
|---|---|---|---|
| `cal.example.com` | `A` | IPv4 address of the server | yes |
| `cal.example.com` | `AAAA` | IPv6 address of the server | if the server has IPv6 |
| `_caldavs._tcp.example.com` | `SRV` | `0 1 443 cal.example.com.` | optional, see below |
| `_caldavs._tcp.example.com` | `TXT` | `path=/dav/` | optional |
| `_carddavs._tcp.example.com` | `SRV` | `0 1 443 cal.example.com.` | optional |
| `_carddavs._tcp.example.com` | `TXT` | `path=/dav/` | optional |
| `example.com` | `CAA` | `0 issue "letsencrypt.org"` | optional, recommended |

In zone-file syntax:

```
cal                 3600 IN A     203.0.113.10
cal                 3600 IN AAAA  2001:db8::10
_caldavs._tcp       3600 IN SRV   0 1 443 cal.example.com.
_caldavs._tcp       3600 IN TXT   "path=/dav/"
_carddavs._tcp      3600 IN SRV   0 1 443 cal.example.com.
_carddavs._tcp      3600 IN TXT   "path=/dav/"
@                   3600 IN CAA   0 issue "letsencrypt.org"
```

- The `A`/`AAAA` records are all you need. Clients find the DAV endpoints
  through `https://cal.example.com/.well-known/caldav` and `…/carddav`, which
  CalCraft answers itself.
- The `SRV`/`TXT` records (RFC 6764) are only useful if users enter their
  e-mail address (`alice@example.com`) instead of the server name, as some
  clients (e.g. Apple) allow. Use them only if `example.com` is your domain.
- Check the records before continuing:

  ```bash
  dig +short cal.example.com A
  ```

  ```bash
  dig +short _caldavs._tcp.example.com SRV
  ```

## 4. Install CalCraft

Choose **one** of the two ways.

### 4a. From source

Install the build tools (Debian/Ubuntu shown):

```bash
sudo apt install -y build-essential git curl
```

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

The repository's `rust-toolchain.toml` selects the right Rust version
automatically. Build the web UI assets and the server:

```bash
git clone https://github.com/Zeitgleich/CalCraft.git
```

```bash
cd CalCraft
```

```bash
rustup target add wasm32-unknown-unknown
```

```bash
cargo install wasm-bindgen-cli --version 0.2.114 --locked
```

```bash
scripts/build-ui.sh
```

```bash
CALCRAFT_UI_REQUIRED=1 cargo build --release --locked -p calcraft-server
```

```bash
sudo install -m 0755 target/release/calcraft /usr/local/bin/calcraft
```

```bash
calcraft --version
```

- The wasm-bindgen CLI version must match the one pinned in
  `crates/calcraft-ui/Cargo.toml`; `scripts/build-ui.sh` checks this.
- Without the UI build step the server still works (DAV and API), but `/`
  shows a placeholder page. A DAV-only binary without web UI and API:
  `cargo build --release --locked -p calcraft-server --no-default-features`.

Create the system user `calcraft` (no login shell, home
`/var/lib/calcraft`) and the directories `/var/lib/calcraft` (`0700
calcraft:calcraft`) and `/etc/calcraft` (`0750 root:calcraft`) with the
sysusers.d and tmpfiles.d files shipped in
[`contrib/systemd/`](../contrib/systemd) (also contained in the Linux release
archives); systemd re-applies them at every boot:

```bash
sudo install -Dm644 contrib/systemd/calcraft.sysusers /etc/sysusers.d/calcraft.conf
```

```bash
sudo install -Dm644 contrib/systemd/calcraft.tmpfiles /etc/tmpfiles.d/calcraft.conf
```

```bash
sudo systemd-sysusers /etc/sysusers.d/calcraft.conf
```

```bash
sudo systemd-tmpfiles --create /etc/tmpfiles.d/calcraft.conf
```

Without systemd: `useradd --system --home /var/lib/calcraft --shell
/usr/sbin/nologin calcraft` and `install -d` the two directories with the
modes above.

Continue with [5. Configure](#5-configure).

### 4b. With Docker

Build the image (it contains the web UI and runs as a non-root user in a
distroless container):

```bash
git clone https://github.com/Zeitgleich/CalCraft.git
```

```bash
cd CalCraft
```

```bash
docker build -t calcraft:latest .
```

Run it with a named volume for the database. Publish the port on
**127.0.0.1 only**, so that it is reachable for the reverse proxy on the same
host but not from the internet:

```bash
docker volume create calcraft-data
```

```bash
docker run -d --name calcraft --restart unless-stopped -p 127.0.0.1:5232:5232 -v calcraft-data:/data -e CALCRAFT_SECURITY__HSTS=true calcraft:latest
```

All settings can be passed as environment variables
(`CALCRAFT_<SECTION>__<KEY>`, see the next section). Create the first user
inside the container:

```bash
docker exec -it calcraft calcraft user add admin --admin --display-name "Administrator"
```

Skip sections 5–7 and continue with [8. TLS certificate](#8-tls-certificate).

## 5. Configure

Write `/etc/calcraft/calcraft.toml` (the full list of options with comments is
in [`calcraft.example.toml`](../calcraft.example.toml)):

```toml
[server]
bind = "127.0.0.1:5232"
max_body_bytes = 10485760
request_timeout_secs = 30
# The reverse proxy (section 9/10) runs on this host: take the client address
# from its X-Forwarded-For header (this is the default).
trusted_proxies = ["127.0.0.1/32", "::1/128"]

[storage]
database = "/var/lib/calcraft/calcraft.db"
# Largest vCard incl. contact photo. Unset: 2 MiB (capped at max_body_bytes);
# an explicit value must not exceed server.max_body_bytes.
# max_vcard_bytes = 2097152

[security]
# CalCraft runs behind HTTPS (see section 9/10), so tell browsers to stay on HTTPS.
hsts = true
secure_cookies = true

[web_ui]
enabled = true

[api]
enabled = true

[logging]
level = "info"
format = "json"
```

```bash
sudo chown root:calcraft /etc/calcraft/calcraft.toml
```

```bash
sudo chmod 0640 /etc/calcraft/calcraft.toml
```

Check the configuration (unknown or misspelled keys are rejected):

```bash
sudo -u calcraft calcraft check-config --config /etc/calcraft/calcraft.toml
```

Notes:

- `bind` must stay on `127.0.0.1` when a reverse proxy runs on the same host.
  CalCraft warns at startup when it listens on a public address without HSTS.
- The database file and its `-wal`/`-shm` companions are created with
  permissions `0600`.
- Every setting can also be set as an environment variable, e.g.
  `CALCRAFT_STORAGE__DATABASE=/var/lib/calcraft/calcraft.db`.
- To disable the web UI or the REST API, set `enabled = false` in the
  respective section; DAV keeps working.
- Request limits are on by default: at most `max_concurrent_requests` (256)
  requests at the same time, further ones get `503` immediately (`/healthz`
  and `/readyz` are exempt); per client 32 concurrent requests, a burst of
  120 and 20 requests per second on average (`429`), and 20 failed logins in
  15 minutes, after which logins from that client fail like a wrong password
  until the window ends. IPv6 clients count per /64. See
  `calcraft.example.toml` for the keys; raise the per-client values if many
  users share one public address.
- `trusted_proxies` decides whose `X-Forwarded-For` header is believed. The
  default is loopback (`127.0.0.1/32`, `::1/128`), which fits the proxy on the
  same host used here. Requests from other peers count for the TCP peer; if
  that is a proxy not listed, all users share one set of per-client limits,
  and CalCraft logs a warning once ("ignoring X-Forwarded-For …"). List
  exactly the proxies in front of CalCraft, and only if they append the
  client address (both configurations below do). Never list networks that
  clients can send requests from directly.
- Trade-off of the default: every local process can send requests with a
  chosen `X-Forwarded-For`, i.e. evade the per-client limits or use up the
  failed logins of another address. On hosts with untrusted local users
  and no local proxy, set `trusted_proxies = []`.
- With Docker (section 4b), the proxy's connections arrive from the bridge
  gateway, usually `172.17.0.1`, which is not trusted by default:
  `-e 'CALCRAFT_SERVER__TRUSTED_PROXIES=["172.17.0.1/32"]'`. Run with
  `CALCRAFT_LOGGING__LEVEL=calcraft_server=debug` once to see the `client`
  field of the request spans and check that it shows real client addresses.

## 6. Run as a systemd service

Install the unit shipped in
[`contrib/systemd/calcraft.service`](../contrib/systemd/calcraft.service)
(user and directories come from section 4a):

```bash
sudo install -Dm644 contrib/systemd/calcraft.service /etc/systemd/system/calcraft.service
```

```bash
sudo systemctl daemon-reload
```

```bash
sudo systemctl enable --now calcraft
```

```bash
systemctl status calcraft
```

```bash
curl -s http://127.0.0.1:5232/readyz
```

The last command must print `ok`. Logs: `journalctl -u calcraft -f`.

What the unit does:

- **Readiness** (`Type=notify`): CalCraft tells systemd `READY=1` only after
  the database is open, migrations are applied and the listener is bound, so
  `systemctl start` returns – and units ordered `After=calcraft.service`
  start – only when CalCraft accepts requests. `systemctl status` shows
  CalCraft's status line (e.g. `Listening on 127.0.0.1:5232`). Startup may
  take up to `TimeoutStartSec=5min` (migrations and the pre-migration backup
  of a large database after an update).
- **Configuration check**: `ExecStartPre=calcraft check-config --quiet`
  rejects a broken configuration before the server starts; the reason is in
  `journalctl -u calcraft`.
- **Restarts**: after a crash, a kill or an out-of-memory stop
  (`MemoryMax`), systemd restarts CalCraft after 5 seconds
  (`Restart=on-failure`). A **watchdog** (`WatchdogSec=30s`) restarts a hung
  server: CalCraft sends a keep-alive every 15 seconds while it is
  responsive. If the service fails 5 times within 5 minutes
  (`StartLimitBurst=5`, `StartLimitIntervalSec=300`), e.g. because of a
  broken configuration, systemd gives up and leaves it `failed` instead of
  restarting it forever. Fix the cause, then:

  ```bash
  sudo systemctl reset-failed calcraft
  ```

  ```bash
  sudo systemctl start calcraft
  ```

  Manual starts count too: more than 5 `systemctl restart` within 5 minutes
  also hit the limit.
- **Graceful shutdown**: `systemctl stop`/`restart` send `SIGTERM`; CalCraft
  stops accepting connections, finishes running requests and exits
  (`TimeoutStopSec=30`, after that systemd kills it).
- **Hardening**: read-only file system except `/var/lib/calcraft`, no
  capabilities, private `/tmp` and devices, system call filter, and resource
  limits (`LimitNOFILE`, `MemoryHigh`/`MemoryMax`, `TasksMax`; adjust them to
  the host). `systemd-analyze security calcraft` shows the exposure level;
  CI checks it (`scripts/systemd-test.sh`).

Change settings with a drop-in instead of editing the unit, so an update of
the shipped file does not overwrite them:

```bash
sudo systemctl edit calcraft
```

for example:

```ini
[Service]
MemoryMax=2G
```

A different binary location needs both `ExecStartPre=` and `ExecStart=`
overridden (an empty `ExecStartPre=` line first clears the shipped one).

After a reboot, check that the service came back:

```bash
systemctl is-enabled calcraft
```

```bash
systemctl is-active calcraft
```

```bash
journalctl -b -u calcraft
```

`is-enabled` must print `enabled` (the unit is wanted by
`multi-user.target`), `is-active` `active`; the journal of the current boot
(`-b`) shows the startup and any errors.

With Docker the unit is not used: the container's restart policy
(`--restart unless-stopped`) takes its place, and CalCraft ignores systemd
notifications when `NOTIFY_SOCKET` is not set.

Health checks (for load balancers, Kubernetes probes or monitoring; both are
exempt from the global concurrency limit, not from the per-client rate limit):

- `GET /healthz` – liveness: `200 ok` while the process serves requests. It
  does not touch the database; do not restart CalCraft because of a storage
  problem.
- `GET /readyz` – readiness: `200 ok` only if the database can be read *and*
  written. Otherwise `503` with `Retry-After: 5` and one of the bodies
  `database not writable` (read-only file or file system), `storage full` (a
  write failed for lack of space and there is still no room) or `database
  unavailable` (the database cannot be read, or fails otherwise, e.g. with
  an I/O error). The write check
  takes the write lock and rolls back; it changes no data and does not grow
  the database. After a full disk it commits and removes a 64 KiB test value
  until that succeeds, so the instance becomes ready again by itself once
  space is freed. If the writer is busy (a burst of writes, waiting longer
  than 1 s, or another process holding the lock for more than 0.5 s), the
  check gives no new information and the last known state is reported (ready
  unless a write failure was recorded). Results are reused for 5 seconds, so
  frequent probes do not compete with clients for the database.

## 7. Create the first user

Create an administrator. The password is prompted for (never pass it as an
argument; it would end up in the shell history). It needs at least 12
characters.

```bash
sudo -u calcraft env CALCRAFT_CONFIG=/etc/calcraft/calcraft.toml calcraft user add admin --admin --display-name "Administrator"
```

Further users can be created in the web UI (admin page) or with the same
command without `--admin`. Other commands: `calcraft user list`,
`passwd`, `disable`, `enable`, `set-admin`.

## 8. TLS certificate

With certbot (Let's Encrypt). Install the plugin for your web server and
request a certificate; certbot renews it automatically.

For nginx:

```bash
sudo apt install -y nginx certbot python3-certbot-nginx
```

```bash
sudo certbot certonly --nginx -d cal.example.com
```

For Apache:

```bash
sudo apt install -y apache2 certbot python3-certbot-apache
```

```bash
sudo certbot certonly --apache -d cal.example.com
```

The certificate ends up in `/etc/letsencrypt/live/cal.example.com/`.

## 9. Reverse proxy: nginx

Create `/etc/nginx/sites-available/calcraft`:

```nginx
# Log without query strings: a single sign-on callback carries one-time codes.
log_format calcraft '$remote_addr - [$time_local] "$request_method $uri $server_protocol" '
                    '$status $body_bytes_sent "$http_user_agent" $request_time';

server {
    listen 80;
    listen [::]:80;
    server_name cal.example.com;

    # Let's Encrypt renewals, everything else to HTTPS.
    location /.well-known/acme-challenge/ {
        root /var/www/html;
    }
    location / {
        return 301 https://$host$request_uri;
    }
}

server {
    listen 443 ssl;
    listen [::]:443 ssl;
    http2 on;
    server_name cal.example.com;

    ssl_certificate     /etc/letsencrypt/live/cal.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/cal.example.com/privkey.pem;
    ssl_protocols       TLSv1.2 TLSv1.3;
    ssl_prefer_server_ciphers off;
    ssl_session_cache   shared:SSL:10m;
    ssl_session_tickets off;

    access_log /var/log/nginx/calcraft.access.log calcraft;
    error_log  /var/log/nginx/calcraft.error.log;

    # Must not be smaller than server.max_body_bytes (10 MiB), which also
    # bounds storage.max_vcard_bytes (contact photos) and HTTP imports.
    client_max_body_size 10m;

    location / {
        proxy_pass http://127.0.0.1:5232;
        proxy_http_version 1.1;

        proxy_set_header Host              $host;
        proxy_set_header X-Forwarded-For   $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        proxy_set_header X-Real-IP         $remote_addr;

        # Exports are streamed; do not buffer them on disk.
        proxy_buffering off;
        proxy_request_buffering on;
        proxy_read_timeout 60s;
        proxy_send_timeout 60s;
    }
}
```

Enable it and reload:

```bash
sudo ln -s /etc/nginx/sites-available/calcraft /etc/nginx/sites-enabled/calcraft
```

```bash
sudo nginx -t
```

```bash
sudo systemctl reload nginx
```

Notes:

- Proxy **everything** under `/` to CalCraft, including `/.well-known/caldav`
  and `/.well-known/carddav` (CalCraft redirects them to `/dav/`) and all DAV
  methods (`PROPFIND`, `REPORT`, `MKCALENDAR`, `MOVE`, `COPY`, …); nginx
  passes them through by default. Do not add `limit_except` or WAF rules that
  block them.
- Keep `proxy_set_header Host $host`: `MOVE` and `COPY` only accept a
  `Destination` on the host the request was sent to (other hosts get `502`).
  If the public URL has a non-default port (e.g. `:8443`), use
  `proxy_set_header Host $http_host` so that the port is passed on.
- Do **not** add a `Strict-Transport-Security` header in nginx; CalCraft sends
  it itself when `security.hsts = true`. Do not add your own
  `Content-Security-Policy` either, CalCraft sets a strict one.
- Do not cache responses (no `proxy_cache`); they are user-specific.
- The `Authorization` header is forwarded by default; do not strip it.
- Keep `X-Forwarded-For $proxy_add_x_forwarded_for`: it appends the address
  nginx received the request from, which CalCraft uses as client address for
  its per-client limits when `server.trusted_proxies` contains nginx's
  address (default `127.0.0.1/32`, `::1/128`, see section 5). Do not replace it with
  `$http_x_forwarded_for`, which passes the client's own value unchecked.
  CalCraft reads `X-Forwarded-For` only, not `X-Real-IP` or `Forwarded`.
- If nginx itself sits behind another proxy or a load balancer, add that one
  to `trusted_proxies` as well (CalCraft skips trusted entries from the right),
  not the `real_ip` module's `set_real_ip_from`.
- nginx older than 1.25.1 (e.g. Debian 12 ships 1.22) does not know
  `http2 on;`: remove that line and write `listen 443 ssl http2;` and
  `listen [::]:443 ssl http2;` instead.

## 10. Reverse proxy: Apache

Enable the required modules:

```bash
sudo a2enmod ssl proxy proxy_http headers rewrite http2
```

Create `/etc/apache2/sites-available/calcraft.conf`:

```apache
# Log without query strings: a single sign-on callback carries one-time codes.
LogFormat "%a - %t \"%m %U %H\" %>s %B \"%{User-Agent}i\" %D" calcraft

<VirtualHost *:80>
    ServerName cal.example.com

    RewriteEngine On
    RewriteCond %{REQUEST_URI} !^/\.well-known/acme-challenge/
    RewriteRule ^ https://%{HTTP_HOST}%{REQUEST_URI} [R=301,L]
</VirtualHost>

<VirtualHost *:443>
    ServerName cal.example.com
    Protocols h2 http/1.1

    SSLEngine on
    SSLCertificateFile    /etc/letsencrypt/live/cal.example.com/fullchain.pem
    SSLCertificateKeyFile /etc/letsencrypt/live/cal.example.com/privkey.pem
    SSLProtocol           -all +TLSv1.2 +TLSv1.3
    SSLHonorCipherOrder   off
    SSLSessionTickets     off

    CustomLog ${APACHE_LOG_DIR}/calcraft.access.log calcraft
    ErrorLog  ${APACHE_LOG_DIR}/calcraft.error.log

    # Must not be smaller than server.max_body_bytes (10 MiB), which also
    # bounds storage.max_vcard_bytes (contact photos) and HTTP imports.
    LimitRequestBody 10485760

    # Keep percent-encoded object names (e.g. a%40b.ics) intact.
    AllowEncodedSlashes NoDecode

    ProxyPreserveHost On
    ProxyRequests     Off
    ProxyTimeout      60
    ProxyPass        / http://127.0.0.1:5232/ nocanon
    ProxyPassReverse / http://127.0.0.1:5232/

    RequestHeader set X-Forwarded-Proto "https"
</VirtualHost>
```

Enable it and reload:

```bash
sudo a2ensite calcraft
```

```bash
sudo apachectl configtest
```

```bash
sudo systemctl reload apache2
```

Notes:

- `nocanon` and `AllowEncodedSlashes NoDecode` make Apache pass request paths
  unchanged; without them, clients with unusual object names can get 404s.
- If `mod_security` or another WAF is active, make sure it allows the DAV
  methods (`PROPFIND`, `PROPPATCH`, `REPORT`, `MKCALENDAR`, `MKCOL`, `MOVE`,
  `COPY`) and XML request bodies for this virtual host.
- Keep `ProxyPreserveHost On`: `MOVE` and `COPY` compare the host in the
  `Destination` header with the `Host` header (other hosts get `502`).
- As with nginx: no own HSTS or CSP headers, no caching, keep `Authorization`.
- `mod_proxy` appends the client address to `X-Forwarded-For` by default
  (`ProxyAddHeaders On`); keep it that way and do not set the header with
  `RequestHeader`. With `server.trusted_proxies = ["127.0.0.1/32", "::1/128"]`
  (the default, section 5) CalCraft then applies its per-client limits to the
  real client.
  Do not combine it with `mod_remoteip` rewriting the header.
- Over HTTP/2, Apache passes oversized uploads on and CalCraft rejects them;
  the client then sees `502` instead of `413`. The request is refused either
  way.

## 11. Firewall

Only 80 and 443 need to be open; port 5232 must stay local. With `ufw`:

```bash
sudo ufw allow 80/tcp
```

```bash
sudo ufw allow 443/tcp
```

```bash
sudo ufw enable
```

Check from another machine that `http://cal.example.com:5232/` is **not**
reachable.

**Outbound connections.** CalCraft itself only connects out for features you
enable: single sign-on (your identity provider), the update check
(`api.github.com`, port 443) and calendar subscriptions (`[subscriptions]`,
off by default). With subscriptions enabled the server fetches the feeds its
users enter. CalCraft refuses private, loopback, link-local and cloud metadata
addresses itself (after resolving the name and for every redirect, see the
README), but an egress firewall is a good second line: allow outbound 443
(and 80 only if you set `allow_http = true`) and block your internal networks
for the CalCraft service user or container, for example:

```bash
# Example for a dedicated service user "calcraft" (nftables/iptables owner match):
sudo iptables -A OUTPUT -m owner --uid-owner calcraft -d 10.0.0.0/8 -j REJECT
```

```bash
sudo iptables -A OUTPUT -m owner --uid-owner calcraft -d 172.16.0.0/12 -j REJECT
```

```bash
sudo iptables -A OUTPUT -m owner --uid-owner calcraft -d 192.168.0.0/16 -j REJECT
```

```bash
sudo iptables -A OUTPUT -m owner --uid-owner calcraft -d 169.254.0.0/16 -j REJECT
```

Keep exceptions you need (e.g. a local identity provider or DNS resolver) in
front of these rules. `allowed_hosts` restricts subscriptions to known feed
providers if your users only need a few.

## 12. Connect clients

1. Open `https://cal.example.com/` in a browser and log in.
2. Recommended: enable two-factor authentication under *Settings*.
3. Create an **app password** for each device under *App passwords*.
   With 2FA enabled, DAV clients only accept app passwords.
4. Configure the client with the server `https://cal.example.com/`, your
   username and the app password:

| Client | Where | Server / URL |
|---|---|---|
| Thunderbird | *New Calendar → On the Network* (and *New Address Book → CardDAV*) | `https://cal.example.com/` |
| iOS / iPadOS | *Settings → Apps → Calendar → Calendar Accounts → Add Account → Other → Add CalDAV Account* (Contacts: *CardDAV Account*) | `cal.example.com` |
| macOS | *System Settings → Internet Accounts → Add Account → Add Other Account → CalDAV/CardDAV account*, account type *Manual* | `cal.example.com` |
| Android | [DAVx⁵](https://www.davx5.com/): *Login with URL and user name* | `https://cal.example.com/` |
| GNOME / Evolution | *Online Accounts → Calendar, Contacts and Files* | `https://cal.example.com/` |

The web UI shows the exact DAV URL of every calendar and address book, for
clients that need it (e.g. `https://cal.example.com/dav/calendars/alice/default/`).

## 13. Backups

All data is in the SQLite database. Back it up while CalCraft is running
(consistent online snapshot, verified, written with permissions `0600`):

```bash
sudo install -d -m 0700 -o calcraft -g calcraft /var/lib/calcraft/backups
```

```bash
sudo -u calcraft env CALCRAFT_CONFIG=/etc/calcraft/calcraft.toml calcraft backup /var/lib/calcraft/backups/calcraft-$(date +%F).db
```

Copy the backups to a safe place (e.g. with your regular backup tool) and
keep several generations. Backups contain password hashes and all calendar
data: store them encrypted and with restricted permissions. A daily cron
example and all details are in [`backup-and-upgrade.md`](backup-and-upgrade.md).

Single calendars or address books can also be exported, e.g. as iCalendar:

```bash
sudo -u calcraft env CALCRAFT_CONFIG=/etc/calcraft/calcraft.toml calcraft export --user alice --collection calendar/default --format ics -o /var/lib/calcraft/alice-default.ics
```

To restore, stop CalCraft (`calcraft restore` refuses to run while the
server has the database open), restore and start it again. The current
database is kept as `calcraft.db.before-restore-<timestamp>`:

```bash
sudo systemctl stop calcraft
```

```bash
sudo -u calcraft env CALCRAFT_CONFIG=/etc/calcraft/calcraft.toml calcraft restore /var/lib/calcraft/backups/calcraft-2026-10-21.db
```

```bash
sudo systemctl start calcraft
```

With Docker, `docker exec calcraft calcraft backup /data/backup.db --force`
writes a backup into the volume; to restore, stop the container and run
`calcraft restore --yes /data/backup.db` in a one-off container on the same
volume.

## 14. Updates

1. Back up the database (section 13). In addition, CalCraft writes a
   backup `calcraft.db.pre-migration-<version>-<timestamp>` next to the
   database before applying migrations (the last 3 are kept).
   If the running CalCraft predates `calcraft backup`, stop it before
   installing the new version: older servers take no database lock.
2. Update the code and rebuild (from source):

   ```bash
   git pull
   ```

   ```bash
   scripts/build-ui.sh
   ```

   ```bash
   CALCRAFT_UI_REQUIRED=1 cargo build --release --locked -p calcraft-server
   ```

   ```bash
   sudo install -m 0755 target/release/calcraft /usr/local/bin/calcraft
   ```

   ```bash
   sudo systemctl restart calcraft
   ```

   With Docker: `git pull`, rebuild the image, then remove and re-create the
   container with the same `docker run` command (the volume keeps the data).

   With a published release instead: download the archive for your platform
   and `SHA256SUMS` from the
   [releases page](https://github.com/Zeitgleich/CalCraft/releases) (or use
   the image `ghcr.io/zeitgleich/calcraft:<version>`), **verify them** as
   described in [`release-verification.md`](release-verification.md)
   (checksums, `gh attestation verify`, `cosign verify` for the image), then
   install `calcraft` from the archive as above and restart the service.
3. Database migrations run automatically at startup. Check
   `journalctl -u calcraft` (`wrote pre-migration backup`,
   `database migrations applied`) and `curl -s http://127.0.0.1:5232/readyz`.

Migrations are forward-only. To roll back, stop the new version, install the
old binary and restore the pre-migration backup; see
[`backup-and-upgrade.md`](backup-and-upgrade.md#rollback).

Security fixes are announced via GitHub security advisories; watch the
repository (*Watch → Custom → Security alerts*).

To see new releases in the web UI's admin area, enable the optional update
notice in `/etc/calcraft/calcraft.toml` and restart:

```toml
[updates]
check = true
# interval_hours = 24   # 1-168
```

CalCraft then sends a single `GET` to
`https://api.github.com/repos/Zeitgleich/CalCraft/releases/latest` at startup
and every `interval_hours` (plus a random delay of up to 10 %). Nothing else is
sent: no cookies, no credentials, no data about the installation. GitHub sees
the server's IP address and the User-Agent `calcraft/<version>`. The server
needs outbound HTTPS (port 443) for this; failures are logged as warnings and
do not affect serving. The notice is off by default and nothing is requested
while it is off.

## 15. Troubleshooting

| Symptom | Cause and fix |
|---|---|
| `502 Bad Gateway` | CalCraft is not running or listens elsewhere: `systemctl status calcraft`, `curl http://127.0.0.1:5232/readyz`. |
| `/readyz` answers `503 storage full` | The disk (or file system quota) is full; writes answer `507`. Free space; the instance becomes ready again within seconds, without a restart. |
| `/readyz` answers `503 database not writable` | The database file, its `-wal`/`-shm` files or the file system are read-only (e.g. remounted read-only after I/O errors, wrong owner). Writes answer `500`, reads keep working. Check `dmesg`, mounts and file ownership. |
| Client reports wrong password, although it is correct | 2FA is enabled: use an app password. After 5 wrong attempts an account is locked for 15 minutes and refuses even the correct password. |
| `413 Request Entity Too Large` (Apache over HTTP/2: `502`) on large uploads or imports | Raise `client_max_body_size` (nginx) / `LimitRequestBody` (Apache) to at least `server.max_body_bytes`. |
| Contacts with large photos do not sync (`403`, `max-resource-size`) | Raise `storage.max_vcard_bytes` (default 2 MiB or `server.max_body_bytes` if smaller; at most 10 MiB and `server.max_body_bytes`). |
| DAV client finds no calendars | Use the server root `https://cal.example.com/` (not a sub-path). Test discovery: `curl -i https://cal.example.com/.well-known/caldav` must answer `301` with `Location: /dav/`. |
| `405 Method Not Allowed` for `PROPFIND`/`REPORT` | A WAF or proxy rule blocks DAV methods; allow them for this host. |
| Moving an event to another calendar fails with `502` | The proxy does not pass the public host name (and port) in `Host`: `proxy_set_header Host $host` (or `$http_host` with a non-default port) for nginx, `ProxyPreserveHost On` for Apache. |
| Objects with special characters in their name return 404 (Apache) | Add `nocanon` and `AllowEncodedSlashes NoDecode` (section 10). |
| Web UI login loops back to the login page | The site must be served over HTTPS (session cookies are `Secure`). For local tests over plain HTTP on another host set `security.secure_cookies = false`. |
| Startup fails with `unknown field` | A configuration key is misspelled; `calcraft check-config` shows which one. |

More help: [GitHub issues](https://github.com/Zeitgleich/CalCraft/issues).
Security problems: see [SECURITY.md](../SECURITY.md).
