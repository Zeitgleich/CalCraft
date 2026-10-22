#!/usr/bin/env bash
# Installs CalCraft as a systemd service with the files from contrib/systemd
# and checks that it registers and survives failures and restarts (used by
# CI on a runner with systemd as PID 1):
#
# - systemd-analyze verify/security of the unit (exposure score)
# - sysusers/tmpfiles create the user and directories with the right modes
# - Type=notify: "active" is reached only after READY=1 (/readyz answers
#   at once, StatusText comes from CalCraft)
# - SIGKILL -> automatic restart (NRestarts), data still there
# - the watchdog keeps a healthy server alive beyond WatchdogSec
# - `systemctl restart` shuts down gracefully
# - enabled for multi-user.target (comes back after a reboot)
# - a broken configuration ends in "failed" (start limit) instead of looping
#
# DESTRUCTIVE for the host it runs on (installs to /usr/local/bin, /etc,
# /var/lib): run it only on a throw-away machine or CI runner, as root.
#
# Usage: sudo scripts/systemd-test.sh <path to calcraft binary>
set -euo pipefail

bin=${1:?usage: systemd-test.sh <path to calcraft binary>}
contrib=$(cd "$(dirname "$0")/../contrib/systemd" && pwd)
unit=calcraft.service
config=/etc/calcraft/calcraft.toml
ready_url=http://127.0.0.1:5232/readyz
password="systemd test password"
max_exposure=3.5

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

step() {
  echo
  echo "=== $*"
}

prop() {
  systemctl show --property="$1" --value "$unit"
}

expect_prop() {
  local actual
  actual=$(prop "$1")
  [ "$actual" = "$2" ] || fail "$1 is '$actual', expected '$2'"
  echo "$1=$actual"
}

# wait_for <seconds> <description> <command...>
wait_for() {
  local seconds=$1 what=$2
  shift 2
  for _ in $(seq 1 "$seconds"); do
    if "$@"; then return 0; fi
    sleep 1
  done
  fail "timed out after ${seconds}s waiting for: $what"
}

as_calcraft() {
  runuser -u calcraft -- env CALCRAFT_CONFIG="$config" /usr/local/bin/calcraft "$@"
}

ready() {
  [ "$(curl -sf --max-time 5 "$ready_url")" = ok ]
}

running() {
  [ "$(prop ActiveState)/$(prop SubState)" = active/running ]
}

journal() {
  journalctl --unit="$unit" --no-pager --output=cat
}

# No grep -q in pipelines: with pipefail, an early exit of grep can fail
# the writer with SIGPIPE.
journal_has() {
  journal | grep -F -- "$1" >/dev/null
}

journal_count() {
  journal | grep -cF -- "$1" || true
}

user_exists() {
  as_calcraft user list | grep -w alice >/dev/null
}

[ "$(id -u)" = 0 ] || fail "must run as root"
[ "$(ps -p 1 -o comm=)" = systemd ] || fail "systemd is not PID 1"
systemctl --version | sed -n 1p

step "install binary and contrib/systemd files"
install -m 0755 "$bin" /usr/local/bin/calcraft
install -Dm644 "$contrib/calcraft.service" /etc/systemd/system/calcraft.service
install -Dm644 "$contrib/calcraft.sysusers" /etc/sysusers.d/calcraft.conf
install -Dm644 "$contrib/calcraft.tmpfiles" /etc/tmpfiles.d/calcraft.conf
systemd-sysusers /etc/sysusers.d/calcraft.conf
systemd-tmpfiles --create /etc/tmpfiles.d/calcraft.conf

step "user and directories"
IFS=: read -r _ _ uid _ _ home shell < <(getent passwd calcraft) || fail "no user calcraft"
echo "calcraft: uid $uid, home $home, shell $shell"
[ "$uid" -lt 1000 ] || fail "calcraft is not a system user (uid $uid)"
[ "$home" = /var/lib/calcraft ] || fail "home is $home"
case "$shell" in
  */nologin | */false) ;;
  *) fail "calcraft has a login shell: $shell" ;;
esac
getent group calcraft >/dev/null || fail "no group calcraft"
[ "$(stat -c '%a %U:%G' /var/lib/calcraft)" = "700 calcraft:calcraft" ] ||
  fail "/var/lib/calcraft: $(stat -c '%a %U:%G' /var/lib/calcraft)"
[ "$(stat -c '%a %U:%G' /etc/calcraft)" = "750 root:calcraft" ] ||
  fail "/etc/calcraft: $(stat -c '%a %U:%G' /etc/calcraft)"

step "configuration"
cat >"$config" <<'EOF'
[server]
bind = "127.0.0.1:5232"

[storage]
database = "/var/lib/calcraft/calcraft.db"

[logging]
level = "info"
format = "pretty"
EOF
chown root:calcraft "$config"
chmod 0640 "$config"
as_calcraft check-config --quiet

step "systemd-analyze verify"
systemctl daemon-reload
systemd-analyze verify /etc/systemd/system/calcraft.service

step "systemd-analyze security"
security=$(systemd-analyze security --no-pager "$unit")
echo "$security"
exposure=$(printf '%s\n' "$security" | sed -n 's/.*Overall exposure level for [^:]*: \([0-9.]*\).*/\1/p')
[ -n "$exposure" ] || fail "no exposure level in the systemd-analyze security output"
echo "exposure level: $exposure (limit $max_exposure)"
if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  echo "systemd-analyze security calcraft.service: overall exposure level **$exposure**" >>"$GITHUB_STEP_SUMMARY"
fi
awk -v e="$exposure" -v m="$max_exposure" 'BEGIN { exit !(e <= m) }' ||
  fail "exposure level $exposure exceeds $max_exposure"

step "enable --now (returns once READY=1 arrived)"
systemctl enable --now "$unit"
# Type=notify: the start job finishes only after READY=1, so the server must
# answer immediately, without any waiting here.
expect_prop Type notify
expect_prop ActiveState active
expect_prop SubState running
expect_prop NRestarts 0
status=$(prop StatusText)
echo "StatusText=$status"
case "$status" in
  "Listening on 127.0.0.1:5232") ;;
  *) fail "StatusText '$status' does not come from CalCraft's READY notification" ;;
esac
ready || fail "/readyz does not answer right after start"
expect_prop WatchdogUSec 30s
[ -e /var/lib/calcraft/calcraft.db ] || fail "database not created"
[ "$(stat -c '%a %U' /var/lib/calcraft/calcraft.db)" = "600 calcraft" ] ||
  fail "database: $(stat -c '%a %U' /var/lib/calcraft/calcraft.db)"

step "enabled for boot"
[ "$(systemctl is-enabled "$unit")" = enabled ] || fail "unit not enabled"
[ -L /etc/systemd/system/multi-user.target.wants/calcraft.service ] ||
  fail "no multi-user.target.wants symlink"
systemctl list-dependencies --plain --no-pager multi-user.target | grep -F "$unit" >/dev/null ||
  fail "multi-user.target does not pull in $unit"

step "create a user (persistence check)"
printf '%s\n' "$password" | as_calcraft user add alice --password-stdin
user_exists || fail "user alice not listed"

step "SIGKILL -> automatic restart"
old_pid=$(prop MainPID)
systemctl kill --signal=KILL "$unit"
wait_for 30 "automatic restart" bash -c "[ \"\$(systemctl show -P NRestarts $unit)\" = 1 ]"
wait_for 30 "service running again" running
[ "$(prop MainPID)" != "$old_pid" ] || fail "MainPID unchanged after SIGKILL"
ready || fail "/readyz not OK after the automatic restart"
user_exists || fail "user alice lost after SIGKILL"
echo "restarted: pid $old_pid -> $(prop MainPID), NRestarts=$(prop NRestarts)"

step "watchdog keeps a healthy server alive (> WatchdogSec)"
pid=$(prop MainPID)
sleep 40
[ "$(prop MainPID)" = "$pid" ] || fail "server was restarted (watchdog?)"
expect_prop NRestarts 1
running || fail "service not running after 40 s"

step "systemctl restart -> graceful shutdown"
shutdowns=$(journal_count "shut down")
start=$(date +%s)
systemctl restart "$unit"
elapsed=$(($(date +%s) - start))
echo "restart took ${elapsed}s"
[ "$elapsed" -lt 25 ] || fail "restart took ${elapsed}s (graceful shutdown should be quick)"
running || fail "not running after restart"
ready || fail "/readyz not OK after restart"
wait_for 10 "'shut down' in the journal" bash -c \
  "[ \"\$(journalctl -u $unit --no-pager -o cat | grep -cF 'shut down' || true)\" -gt $shutdowns ]"
journal_has "shutdown signal received" || fail "no 'shutdown signal received' in the journal"
if journal | grep -iE "stop-sigterm.*timed out|Killing process" >/dev/null; then
  fail "systemd had to kill CalCraft on stop"
fi
user_exists || fail "user alice lost after restart"

step "stop and start (as on reboot)"
systemctl stop "$unit"
expect_prop ActiveState inactive
expect_prop Result success
systemctl start "$unit"
running || fail "not running after start"
ready || fail "/readyz not OK after start"
user_exists || fail "user alice lost after stop/start"

step "broken configuration -> failed after the start limit, no endless loop"
systemctl reset-failed "$unit"
cp "$config" "$config.good"
echo 'not_a_key = 1' >>"$config"
systemctl restart "$unit" && fail "restart with a broken configuration succeeded"
wait_for 120 "Result=start-limit-hit" bash -c "[ \"\$(systemctl show -P Result $unit)\" = start-limit-hit ]"
expect_prop ActiveState failed
restarts=$(prop NRestarts)
sleep 15
expect_prop ActiveState failed
expect_prop NRestarts "$restarts"
journal_has "unknown field" || fail "the configuration error is not in the journal"
echo "gave up after NRestarts=$restarts; journal names the error:"
journal | grep -F "unknown field" | tail -n 1

step "fixed configuration -> starts again"
mv "$config.good" "$config"
systemctl reset-failed "$unit"
systemctl start "$unit"
running || fail "not running after fixing the configuration"
ready || fail "/readyz not OK after fixing the configuration"
user_exists || fail "user alice lost"

echo
echo "systemd test passed (exposure level $exposure)"
