# Repository Guidelines

## Project Goals
- Build a fast, responsive two-panel file manager using the gpui library fork at `https://github.com/gpui-ce/gpui-ce`.
- Prioritize low-latency directory navigation, smooth scrolling, and non-blocking I/O.

## Project Structure & Module Organization
- `src/main.rs` contains the application entry point and core UI logic.
- `themes/` holds external theme definitions (example: `themes/light.toml`, `themes/dark.json`).
- `Cargo.toml` defines crate metadata and dependencies; `Cargo.lock` pins versions.
- `target/` is build output (generated).

## Build, Test, and Development Commands
- `cargo build` builds the binary in debug mode.
- `cargo run` builds and runs the app locally.
- `RUST_LOG=info cargo run` enables runtime logging via `env_logger`.
- `cargo build --release` produces an optimized release binary.
- `cargo test` runs tests (currently no test modules are present).
- `cargo fmt` formats Rust sources; run before committing.
- `cargo clippy -- -D warnings` runs lint checks and fails on warnings.
- If updating gpui, use the `gpui-ce` fork in `Cargo.toml` and note any API changes in the PR.

## Coding Style & Naming Conventions
- Rust 2024 edition, 4-space indentation, trailing commas in multi-line lists.
- Use `snake_case` for functions/variables, `UpperCamelCase` for types, and `SCREAMING_SNAKE_CASE` for constants.
- Prefer small, focused functions; keep UI-specific constants near their usage (e.g., `VIEW_ROWS`).

## Testing Guidelines
- Tests should live in `src/` modules (unit tests) or `tests/` (integration tests) if added.
- Name test functions with descriptive `snake_case` (example: `loads_theme_from_json`).
- Run `cargo test` before opening a PR.

## Commit & Pull Request Guidelines
- Use short, imperative commit subjects (example: "Add theme toggle").
- PRs should describe the change, include reproduction steps if behavior changes, and link related issues if they exist.
- Add screenshots or brief GIFs for UI changes when feasible.

## Configuration & Themes
- External themes are loaded from `themes/`; keep file names lowercase and extension-specific (`.toml`, `.json`).
- When adding a theme, update any selection logic in `src/main.rs` as needed.
