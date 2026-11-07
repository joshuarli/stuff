# Migrating mycrates state to `stuff`

`stuff publish cargo` keeps its durable state in `$STUFF_STATE_HOME/cargo`
(default `${XDG_STATE_HOME:-$HOME/.local/state}/stuff/cargo`). The Python
`mycrates` CLI keeps its state in a `state/` directory that defaults to
`$PWD/.work/state`. The first cargo publish with `stuff` copies an existing
mycrates state into the stuff state before the transaction starts.

## Where the legacy state is read from

1. `STUFF_MYCRATES_STATE`, when set, is the mycrates state directory itself
   (the directory holding `blobs/`, `native/`, `upstream/`, and `pins/`).
2. Otherwise the canonical wrapper location is used when it exists:
   `$STUFF_STATE_HOME/mycrates/state` (default
   `${XDG_STATE_HOME:-$HOME/.local/state}/stuff/mycrates/state`).
3. Otherwise `$PWD/.work/state`, the Python CLI's default.

Set `STUFF_MYCRATES_STATE` when the state lives elsewhere, including when a
checkout was published from a different working directory.

## What is copied

| From the mycrates state | To the stuff state |
| --- | --- |
| `blobs/<sha256>.crate` | `blobs/<sha256>.crate` |
| `native/<name>/<version>.json` | `native/<name>/<version>.json` |
| `upstream/<name>/<version>.json` | `upstream/<name>/<version>.json` |
| `pins/<project>.json` | `pins/<project>.json` |
| `native/**/*.toml`, `pins/*.toml` | the same files converted to `.json` |

Legacy TOML records from the old Rust `mycrates` are converted the same way
`mycrates migrate-state` converts them. `config.json`, `lock`, and `uploads/`
are not copied: `stuff` reads its configuration from
`$STUFF_CONFIG_HOME/config.json` and keeps its own lock files.

## Backup and rollback

Before copying, `stuff` writes a complete copy of the legacy state to:

```
$STUFF_STATE_HOME/cargo/mycrates-backup/
```

The backup is created once. The original mycrates directory is only read; it
is never modified, moved, or deleted. To roll back, stop using `stuff publish
cargo` and keep using the Python CLI against the original state (or point
`STUFF_MYCRATES_STATE` at it). To abandon the migration, delete the stuff
cargo state directory, or restore the files from `mycrates-backup/`.

## Idempotency and conflicts

Migration runs on every cargo publish and is safe to rerun. Files that already
exist with identical bytes are skipped, so a second run copies nothing. If a
destination file exists with different bytes, the publish stops with a conflict
(exit 3) and the destination is left untouched; nothing is overwritten.

Adopted live index records are not stored during migration. Every publish
rebuilds the published set from the live generation, so the migrated records
matter for locally published versions that the live generation does not serve
yet, and for pins and upstream provenance.
