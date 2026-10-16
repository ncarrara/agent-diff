# agent-diff

Desktop app (Rust 2024 + egui/eframe) to watch a remote workspace where Claude
is working, review what changed, and sync whole files or single hunks between
it and a local git workspace, in both directions. Each local/remote pair is a
workspace tab, with an embedded terminal running `claude` on the remote.

See `README.md` for user-facing behavior; keep it up to date when behavior changes.

## Rules

- **Never write comments.** No `//`, `///` or `//!` comments in code you add or
  change. Use clear names and small functions instead.
- **Don't run the tests, don't build** (no `cargo test`, `cargo build`, `cargo check`): the
  pre-commit hook (`.pre-commit-config.yaml`, installed with `uvx pre-commit install`)
  runs the tests.

## Commands

- Build: `cargo build --release` (`.cargo/config.toml` limits to one job for low-memory machines)
- Without some previews: `cargo build --release --no-default-features --features images,sounds`
  (features: `images`, `svg`, `html`, `markdown`, `sounds`, `models`, `bash`, `python`); every combination must keep
  building when touching `previews/`.
- Test: `cargo test` (run by the pre-commit hook)
- Run: `cargo run --release -- LOCAL REMOTE [--interval SECS]`
