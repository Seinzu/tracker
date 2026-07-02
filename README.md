# Tracker

A small macOS-friendly time tracker built with Rust, Tauri, and SQLite.

The first slice supports:

- timing free-form tasks
- optionally associating tasks with a GitHub issue or pull request
- searching GitHub issues and pull requests with an optional token
- assigning a subtask to a timer entry
- stopping the current timer from the app or tray
- storing task, subtask, and time entry data in SQLite
- a starter reporting view grouped by task and subtask

## Run

This app uses a static frontend, so Node/npm are not required.

```sh
cargo run --manifest-path src-tauri/Cargo.toml
```

The SQLite database is created in the app data directory as `tracker.sqlite3`.

GitHub search works without a token for public repositories. Add a token in the app when you need higher rate limits or access to private repositories; the token is stored in the OS credential store, which is macOS Keychain on a Mac, and is not written to SQLite.

## Release

The GitHub Actions workflow at `.github/workflows/build-macos-release.yml` builds macOS release assets for Apple Silicon and Intel Macs.

Create a tag to trigger a draft release:

```sh
git tag v0.1.0
git push origin v0.1.0
```

You can also run the workflow manually from GitHub Actions and provide a release tag.

## Notes

The reporting screen is intentionally simple at this stage. The backend already exposes summary rows and recent entries, so a richer reports UI can be added without changing the storage model.
