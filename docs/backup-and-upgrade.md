# Backups, restore and upgrades

All CalCraft data – accounts, password hashes, calendars, address books,
sync state – lives in one SQLite database (`storage.database`, default
`calcraft.db`). This document describes how to back it up, how to restore it
and how upgrades and rollbacks work. Commands use the paths of
[`installation.md`](installation.md) (`/etc/calcraft/calcraft.toml`,
`/var/lib/calcraft/calcraft.db`, user `calcraft`).

> Backups contain password hashes, TOTP secrets and all calendar data. Store
> them encrypted and with restricted permissions. CalCraft writes every backup
> with permissions `0600`.

## Backup

```bash
sudo -u calcraft env CALCRAFT_CONFIG=/etc/calcraft/calcraft.toml \
  calcraft backup /var/lib/calcraft/backups/calcraft-$(date +%F).db
```

```text
backup written to /var/lib/calcraft/backups/calcraft-2026-10-21.db (245760 bytes, migration 4)
```

- **Online:** the server may keep running. The backup uses SQLite's
  `VACUUM INTO`, which copies the database within a single read
  transaction: the result is a consistent snapshot, including changes that
  are still in the write-ahead log, while clients keep writing.
- **Atomic:** the copy is written to a temporary file in the target
  directory, checked (`PRAGMA integrity_check`, migration table readable) and
  only then renamed to the target name. An interrupted backup never leaves a
  truncated file under the target name.
- **Self-contained:** the backup is a single file (no `-wal`/`-shm`).
- An existing file is not replaced unless `--force` is given.
- `calcraft backup` never migrates the database, so a newer binary can back
  up the database of an older running server.
- The target must not be the database itself or one of its files (`-wal`,
  `-shm`, `-journal`, `.lock`), not even with `--force`.
- Temporary files (`.calcraft.db.tmp-*`, `.calcraft.db.pre-migration-*.tmp-*`)
  left behind by a crash are deleted at the next start once they are older
  than one hour.

Example timer for a daily backup with 14 generations (`/etc/cron.daily/calcraft-backup`):

```bash
#!/bin/sh
set -e
dir=/var/lib/calcraft/backups
install -d -m 0700 -o calcraft -g calcraft "$dir"
sudo -u calcraft env CALCRAFT_CONFIG=/etc/calcraft/calcraft.toml calcraft backup "$dir/calcraft-$(date +%F).db" --force
ls -1t "$dir"/calcraft-*.db | tail -n +15 | xargs -r rm --
```

Copy the backup directory to another machine with your regular (encrypting)
backup tool. Do not copy `calcraft.db` itself while the server runs: without
its `-wal` file the copy can be inconsistent.

With Docker, run the same command in the container, writing to the volume:

```bash
docker exec calcraft calcraft backup /data/backup.db --force
```

## Restore

The server must be stopped. CalCraft refuses to restore while any CalCraft
process (server or CLI command) has the database open:

```bash
sudo systemctl stop calcraft
```

```bash
sudo -u calcraft env CALCRAFT_CONFIG=/etc/calcraft/calcraft.toml \
  calcraft restore /var/lib/calcraft/backups/calcraft-2026-10-21.db
```

```bash
sudo systemctl start calcraft
```

`calcraft restore` asks for confirmation (`--yes` skips it, required when not
run in a terminal) and then:

1. takes the exclusive database lock (see below), or fails if the server
   still runs;
2. copies the backup next to the database (the backup itself is never
   modified) and verifies it: `PRAGMA integrity_check`, migration table, and
   **no migrations unknown to this version** – a backup made by a newer
   CalCraft version must be restored with that version;
3. keeps the current database as `calcraft.db.before-restore-<timestamp>`
   (`0600`, after a WAL checkpoint, so the copy is complete);
4. removes stale `calcraft.db-wal` / `calcraft.db-shm` files, so an old
   write-ahead log is never replayed into the restored database;
5. atomically renames the verified copy to `calcraft.db` (`0600`).

If keeping the copy fails (e.g. disk full), the partial copy is removed and
the live database is not touched. If a later step fails, the error names the
kept copy.

If the backup is older than the binary, the next start migrates it (and
writes a pre-migration backup first, see below). Sync tokens, ETags and
everything else come back exactly as they were at backup time; clients
notice the changed collection state on their next sync.

Delete `calcraft.db.before-restore-*` once the restored state is confirmed.

### How CalCraft detects a running server

Every CalCraft process that opens the database holds a **shared** OS file
lock on `calcraft.db.lock` next to it (`flock` on Linux/macOS, `LockFileEx`
on Windows). `calcraft restore` and migrations need the **exclusive** lock.
The operating system releases the lock when the process ends – also after a
crash or `kill -9` – so there are no stale PID files to clean up. Do not
delete `calcraft.db.lock`; it is empty and harmless. Each process opens the database only
while it holds the lock; to switch between shared and exclusive (which the OS
cannot do atomically) it closes all connections first and reopens the
database afterwards, so a restore can never replace the file under an open
connection. The lock is advisory: it
protects against CalCraft processes, not against other programs (e.g. the
`sqlite3` shell) writing to the database. Network file systems may not
implement locks reliably; keep the database on a local disk.

## Upgrades and migrations

Database migrations run automatically at startup (also when a CLI command
such as `calcraft user list` opens the database; its log goes to standard
error). A non-empty SQLite file without CalCraft's migration table (e.g. a
wrong `storage.database` path) is refused instead of migrated. Before
applying migrations to an existing database, CalCraft

1. takes the exclusive lock – if an older server (or another CalCraft
   command) still uses the database, startup fails with *"needs migrations,
   but another CalCraft process is using it"*; stop it and start again;
2. writes a verified backup
   `calcraft.db.pre-migration-<old version>-<timestamp>` (`0600`) next to the
   database, e.g. `calcraft.db.pre-migration-0004-20261021T120000Z`;
3. applies the migrations;
4. only then deletes older pre-migration backups beyond the configured
   number (the backup just written is always kept).

If the backup cannot be written (disk full, permissions), **nothing is
migrated** and the server does not start:

```text
backup before migrating failed, nothing was migrated: …
```

Configuration (`[storage]`):

| Key | Default | Meaning |
|---|---|---|
| `pre_migration_backup` | `true` | Write the backup before migrating. Only disable this if you take your own backup before every update. |
| `pre_migration_backups_keep` | `3` | Number of pre-migration backups to keep (at least 1). |

Pre-migration backups are a safety net for rollbacks, not a replacement for
regular backups: they are on the same disk as the database.

Update procedure (from source; with Docker rebuild the image instead):

0. **Stop the old server first if it is older than the version that
   introduced `calcraft backup`/`restore`:** such servers take no database
   lock, so the new version cannot detect them and would migrate under them.
   (`sudo systemctl stop calcraft`, install, `sudo systemctl start calcraft`.)
1. `calcraft backup …` (optional, but recommended in addition to the
   automatic pre-migration backup, and copy it off the machine);
2. build and install the new binary (see `installation.md`, "Updates");
3. `sudo systemctl restart calcraft`;
4. check `journalctl -u calcraft` for `wrote pre-migration backup` and
   `database migrations applied`, and `curl -s http://127.0.0.1:5232/readyz`.

## Rollback

Migrations are **forward-only**: there are no down migrations, and an older
CalCraft refuses to start on a database that a newer version has migrated:

```text
error: failed to open database /var/lib/calcraft/calcraft.db: the database was
migrated by a newer CalCraft version (migration 6, this version knows up to 4).
Downgrading in place is not supported: run the newer version again, or restore
the pre-migration backup (calcraft.db.pre-migration-*) with this version, see
docs/backup-and-upgrade.md
```

To roll back an upgrade:

1. Stop the new version: `sudo systemctl stop calcraft`.
2. Reinstall the old binary (`sudo install -m 0755 calcraft-old /usr/local/bin/calcraft`).
3. Restore the pre-migration backup written by the new version on its first
   start. Use the **old** binary if it has `calcraft restore` (any version
   with this document); `calcraft restore` refuses backups newer than the
   binary, and the pre-migration backup has the old schema:

   ```bash
   ls -1t /var/lib/calcraft/calcraft.db.pre-migration-*
   ```

   ```bash
   sudo -u calcraft env CALCRAFT_CONFIG=/etc/calcraft/calcraft.toml \
     calcraft restore --yes /var/lib/calcraft/calcraft.db.pre-migration-0004-20261021T120000Z
   ```

   If the old version predates `calcraft restore`, copy the file by hand
   with the server stopped (this is what `restore` does, without the
   checks):

   ```bash
   cd /var/lib/calcraft
   sudo cp -p calcraft.db calcraft.db.before-rollback
   [ -s calcraft.db-wal ] && sudo cp -p calcraft.db-wal calcraft.db.before-rollback-wal
   sudo rm -f calcraft.db-wal calcraft.db-shm
   sudo install -m 0600 -o calcraft -g calcraft calcraft.db.pre-migration-0004-20261021T120000Z calcraft.db
   ```

4. Start the old version: `sudo systemctl start calcraft`.

Changes made while the new version was running (new events, password
changes, …) are lost by the rollback; they remain in the
`calcraft.db.before-restore-*` copy, which only the new version can open.

A migration that failed half-way (power loss, disk full) is reported at
startup as *"migration N failed earlier and left the database in an unknown
state"*; restore the pre-migration backup in the same way, then retry the
upgrade.

## Tests

- `crates/calcraft-store/src/backup/tests.rs`: consistent backup during
  concurrent writes, restore round trip (items, ETags, sync revision, token
  salt, change log), refusal of newer backups and of restores while the
  database is open (also right after migrating), pre-migration backup when
  opening an old schema, upgrade from every older migration state to the
  current one, retention (never deleting the new backup, ignoring empty
  files), refusal of newer/failed schemas and of foreign SQLite files, no
  migration when the backup fails, protected backup targets, removal of
  stale temporary files, permissions.
- `crates/calcraft-server/tests/backup.rs`: the same with the `calcraft`
  binary, including a running `calcraft serve`.
- CI job `backup` (`.github/workflows/ci.yml`): creates a user, uploads an
  event via HTTP, backs up, deletes the event, stops the server, restores,
  starts it again and fetches the event.
