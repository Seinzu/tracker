# Tracker

A small macOS-friendly time tracker built with Rust, Tauri, and SQLite.

The first slice supports:

- timing free-form tasks
- optionally associating tasks with a GitHub issue or pull request
- searching GitHub issues and pull requests with an optional token
- assigning a subtask to a timer entry, drawn from one list shared by every task
- renaming, merging, and archiving subtasks
- stopping the current timer from the app or tray
- storing task, subtask, and time entry data in SQLite
- reports grouped by task, by subtask, or by both

Subtasks are independent of tasks. A subtask such as `Code review` or `Deploy`
exists once and is recorded against whichever task you are timing, so the
Subtask report answers questions like "how long did I spend reviewing code this
month" across everything you worked on.

## Run

This app uses a static frontend, so Node/npm are not required.

```sh
cargo run --manifest-path src-tauri/Cargo.toml
```

The SQLite database is created in the app data directory as `tracker.sqlite3`.

GitHub search works without a token for public repositories. Add a token in the app when you need higher rate limits or access to private repositories; the token is stored in the OS credential store, which is macOS Keychain on a Mac, and is not written to SQLite.

## Database

The database records its own schema version in SQLite's `user_version` field.
On launch, before anything is read, Tracker compares that version with the one
the build expects and applies any missing migrations in a single transaction.
Installations that predate versioning report version 0; they are recognised by
the tables they already contain and treated as version 1.

Inspect the version of an existing database with:

```sh
sqlite3 ~/Library/Application\ Support/dev.local.tracker/tracker.sqlite3 'PRAGMA user_version'
```

Before migrating a database that holds data, Tracker copies it alongside the
original as `tracker-backup-v<from>-<timestamp>.sqlite3`. A migration that fails
rolls back, leaving the database on its previous version, and the app reports
what went wrong and where the backup is instead of loading. These backups are
never cleaned up automatically; delete them once you are happy with the upgrade.

A database written by a newer build of Tracker is refused rather than opened, so
downgrading cannot corrupt it.

### Version history

| Version | Change |
| ------- | ------ |
| 1 | Tasks, per-task subtasks, and time entries. Everything before schema versioning. |
| 2 | Subtasks became a single shared list, keyed on name rather than owned by a task. |

Migration 2 rebuilds `subtasks` without its `task_id` column and repoints every
time entry at the shared rows. Names that differed only in case or whitespace
across tasks collapse into one subtask, keeping the earliest spelling, so time
recorded against `Review` and `review` is reported together. No time entry is
dropped: an entry whose subtask had a blank name simply loses the label and
keeps its recorded time.

## Release

The GitHub Actions workflow at `.github/workflows/build-macos-release.yml` builds macOS release assets for Apple Silicon and Intel Macs.

If Apple signing secrets are not configured, the workflow creates an ad-hoc signed macOS build. This is usable for personal/internal testing, but macOS Gatekeeper will still block it on first launch because it is not notarized. Use Finder's right-click `Open` action, or remove quarantine after installing:

```sh
xattr -dr com.apple.quarantine /Applications/Tracker.app
```

For a polished release that opens normally after download, add these repository secrets before cutting a release:

- `APPLE_CERTIFICATE`: base64 encoded `.p12` export of a Developer ID Application certificate
- `APPLE_CERTIFICATE_PASSWORD`: password used when exporting the `.p12`
- `APPLE_ID`: Apple ID email for notarization
- `APPLE_PASSWORD`: app-specific password for that Apple ID
- `APPLE_TEAM_ID`: Apple Developer Team ID
- `KEYCHAIN_PASSWORD`: temporary CI keychain password

Convert an exported `.p12` certificate for `APPLE_CERTIFICATE` with:

```sh
openssl base64 -A -in /path/to/certificate.p12 -out certificate-base64.txt
```

Create a tag to trigger a draft release:

```sh
git tag v0.1.0
git push origin v0.1.0
```

You can also run the workflow manually from GitHub Actions and provide a release tag.

## Notes

The reporting screen is intentionally simple at this stage. The backend already exposes summary rows and recent entries, so a richer reports UI can be added without changing the storage model.

Subtask names are matched case-insensitively using Rust's Unicode-aware
lowercasing, with runs of whitespace collapsed. Typing a name that already
exists reuses that subtask, and reuses it even if it had been archived.
