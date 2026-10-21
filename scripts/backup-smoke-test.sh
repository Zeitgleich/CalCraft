#!/usr/bin/env bash
# Backup/restore smoke test with the real binary (used by CI):
# create user, PUT an event, back up, delete the event, stop, restore, start,
# GET the event again.
#
# Usage: scripts/backup-smoke-test.sh <path to calcraft> <empty work dir>
# The database path is taken from CALCRAFT_STORAGE__DATABASE (default:
# <work dir>/calcraft.db).
set -euo pipefail

bin=$1
work=$2
mkdir -p "$work"
export CALCRAFT_STORAGE__DATABASE=${CALCRAFT_STORAGE__DATABASE:-$work/calcraft.db}
unset CALCRAFT_CONFIG
db=$CALCRAFT_STORAGE__DATABASE
port=${CALCRAFT_SMOKE_PORT:-5239}
base=http://127.0.0.1:$port
event=$base/dav/calendars/alice/default/smoke.ics
password="smoke test password"
log=$work/server.log
pid=

fail() {
  echo "FAIL: $*" >&2
  exit 1
}

start() {
  "$bin" serve --bind "127.0.0.1:$port" >>"$log" 2>&1 &
  pid=$!
  for _ in $(seq 1 30); do
    curl -sf "$base/readyz" >/dev/null && return 0
    sleep 1
  done
  fail "server did not become ready"
}

stop() {
  kill "$pid"
  wait "$pid" || true
  pid=
}

cleanup() {
  if [ -n "$pid" ]; then kill "$pid" 2>/dev/null || true; fi
}
trap cleanup EXIT

mode() {
  stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"
}

echo "$password" | "$bin" user add alice --password-stdin
start

cat >"$work/smoke.ics" <<'ICS'
BEGIN:VCALENDAR
VERSION:2.0
PRODID:-//CalCraft//smoke test//EN
BEGIN:VEVENT
UID:backup-smoke-test
DTSTAMP:20261021T120000Z
DTSTART:20261022T100000Z
DTEND:20261022T110000Z
SUMMARY:Backup smoke test
END:VEVENT
END:VCALENDAR
ICS
status=$(curl -s -o /dev/null -w '%{http_code}' -u "alice:$password" \
  -H 'Content-Type: text/calendar' --data-binary @"$work/smoke.ics" -X PUT "$event")
[ "$status" = 201 ] || fail "PUT returned $status"
etag=$(curl -sf -u "alice:$password" -o /dev/null -D - "$event" | tr -d '\r' | awk 'tolower($1) == "etag:" { print $2 }')
[ -n "$etag" ] || fail "no ETag"

# Online backup while the server runs.
"$bin" backup "$work/backup.db"
[ "$(mode "$work/backup.db")" = 600 ] || fail "backup is not 0600"
if "$bin" backup "$work/backup.db" 2>/dev/null; then fail "backup overwrote a file without --force"; fi

status=$(curl -s -o /dev/null -w '%{http_code}' -u "alice:$password" -X DELETE "$event")
[ "$status" = 204 ] || fail "DELETE returned $status"
status=$(curl -s -o /dev/null -w '%{http_code}' -u "alice:$password" "$event")
[ "$status" = 404 ] || fail "event still there after DELETE ($status)"

# Restore is refused while the server runs.
if "$bin" restore --yes "$work/backup.db" 2>"$work/restore.err"; then fail "restore succeeded while the server runs"; fi
grep -q "in use" "$work/restore.err" || fail "unexpected error: $(cat "$work/restore.err")"

stop
"$bin" restore --yes "$work/backup.db"
[ "$(mode "$db")" = 600 ] || fail "restored database is not 0600"
ls "$db".before-restore-* >/dev/null || fail "previous database was not kept"
start

body=$(curl -sf -u "alice:$password" "$event") || fail "GET after restore failed"
echo "$body" | grep -q "UID:backup-smoke-test" || fail "restored event has wrong content"
restored_etag=$(curl -sf -u "alice:$password" -o /dev/null -D - "$event" | tr -d '\r' | awk 'tolower($1) == "etag:" { print $2 }')
[ "$restored_etag" = "$etag" ] || fail "ETag changed: $etag -> $restored_etag"
stop
echo "backup/restore smoke test passed"
