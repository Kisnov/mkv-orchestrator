# Versioning and Migrations

## Product version

Release tags use `vMAJOR.MINOR.PATCH`, such as `v1.1.0`. The version now lives
in three places: `version` under `[workspace.package]` in the root `Cargo.toml`,
`version` in `apps/desktop/src-tauri/tauri.conf.json` (which sets the installer
and window metadata), and `web/package.json` with its lock file.

All three currently use `0.1.0`. Update them together for every release.

## Settings schema

`AppSettings::schema_version` tracks the persisted settings document schema.

Current version:

```text
2
```

Future settings-schema changes must add a Rust migration step before returning
the document and a fixture covering the previous persisted representation.

## Database schema

SQLite migrations in `mkvo-infra-sqlite` track the schema of the whole
database (metadata cache, settings, plans, jobs, journals, logs and rename
history).

The current version is `SCHEMA_VERSION` in
`crates/mkvo-infra-sqlite/src/lib.rs`. The database stores its current migration number in:

```sql
PRAGMA user_version
```

Each numbered step runs in its own transaction and sets `user_version` to its
own number, so an interrupted upgrade resumes from the last completed step.

Before migrating an existing database, MKVO copies it next to itself as
`<database>.v<old version>.bak` (with `VACUUM INTO`, so the copy is consistent
and compacted). The copy needs free space roughly the size of the database; if
it cannot be written, the database is not migrated and startup fails. A copy
left by an earlier upgrade from the same version is replaced.

A database with a newer `user_version` than this build supports is refused
rather than read. To go back to an older build after an upgrade, stop MKVO,
delete `mkvo.db-wal` and `mkvo.db-shm` (a leftover WAL would be applied to the
restored file), and copy the `.bak` of the version that build uses over
`mkvo.db`. Anything recorded since the upgrade is lost.

Future changes should add a numbered migration in `mkvo-infra-sqlite`.
Destructive cache rebuilds are acceptable only for rebuildable scan metadata;
settings, plans, journals, job history, and secrets must be migrated without
silent data loss.

## Retention and file size

Finished jobs, journals and operation logs older than
`rename.history_retention_days` are pruned at startup and daily (zero keeps
them), and rename batches after 30 days. Each scan drops cached probes of files
gone for 7 days from the scanned folders, and of any file not seen for 90 days.
SQLite reuses the freed pages but does not shrink the file. After the first
prune of a database written by older builds (which stored scan results and
per-file events), the file can stay much larger than its data. To reclaim the
space once, stop MKVO and run, in the config directory:

```sh
sqlite3 mkvo.db "VACUUM"
```

It needs free space about the size of the database and blocks it while it
runs, which is why MKVO does not do it on its own.
