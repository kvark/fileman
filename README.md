# Fileman

Fileman is a fast, responsive two-panel file manager built with the gpui UI framework. The goal is to keep navigation snappy even in large directories by doing I/O off the UI thread and streaming results into the view.

## Highlights
- Dual-panel layout with independent navigation.
- Non-blocking directory loading and virtualized list rendering.
- Optional previews for text and images.
- External themes in `themes/` (JSON, YAML, or TOML).

## Build and Run
```bash
cargo build
cargo run
RUST_LOG=info cargo run
```

## Project Notes
- gpui dependency comes from the `gpui-ce` fork: `https://github.com/gpui-ce/gpui-ce`.
- UI responsiveness is a primary requirement; avoid long-running work on the main thread.

## Repository Layout
- `src/main.rs` contains the app entry point and core UI logic.
- `themes/` stores theme files.
