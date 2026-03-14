# Fileman

Fileman is a fast, responsive two-panel file manager built with Rust, egui, and blade-egui. Navigation stays snappy even in large directories by doing all I/O off the UI thread and streaming results into the view.

![Fileman screenshot](etc/snapshots/tests/preview.png)

## Features
- **Dual-panel layout** with independent navigation, history (Alt+Left/Right), and panel swap (Ctrl+U).
- **Async directory loading** with streaming batches and virtualized list rendering.
- **Archive navigation** for zip, tar, tar.gz, and tar.bz2 — browse like regular folders, copy files out, or open with system apps. Shared streaming index means only one decompression pass per archive.
- **Preview** (F3) for text files with syntax highlighting, images (including animated GIF), and archive content listings.
- **Inline editor** (F4) with syntax highlighting.
- **File operations**: copy (F5), move (F6), delete (F8), rename (Shift+F6), new file (Shift+F4), new directory (F7).
- **Multi-file selection**: Insert to mark/unmark, operations work on marked set.
- **Search** (Alt+F7) with results displayed as a virtual folder.
- **Symlink display**: shows link targets, broken symlinks highlighted in red.
- **File properties** (Alt+Enter): permissions, ownership, size, timestamps.
- **Theming**: external theme files in `themes/` (JSON, YAML, or TOML), toggle with F9, pick with F10.
- **CLI**: optional start directory argument, `--help` for usage.

## Keyboard Shortcuts
| Key | Action |
|-----|--------|
| Enter | Open |
| Shift+Enter | Open with system default app |
| Tab / Ctrl+I | Switch panels |
| Ctrl+U | Swap panels |
| Alt+Left / Alt+Right | Back / forward |
| Backspace / Ctrl+PgUp | Parent folder |
| Ctrl+PgDn | Open selected |
| Ctrl+Left / Ctrl+Right | Open selected dir in other panel |
| F1 | Help |
| F3 | Preview |
| F4 | Edit |
| Shift+F4 | New file |
| F7 | New directory |
| Insert | Mark / unmark |
| Shift+F6 | Rename |
| F5 | Copy |
| F6 | Move |
| F8 | Delete |
| Space | Compute folder size |
| Alt+F7 | Search |
| Alt+Enter | Properties |
| Ctrl+R | Refresh |
| F9 | Toggle theme |
| F10 | Theme picker |

## Build and Run
```bash
cargo build --release
cargo run --release
```

Open in a specific directory:
```bash
cargo run --release -- /path/to/dir
```

Enable verbose logging:
```bash
RUST_LOG=info cargo run
```

### GPU Backend Notes
If you see `NoSupportedDeviceFound`, blade-graphics couldn't find a supported GPU backend.
On Linux, this usually means Vulkan drivers aren't available. You can either install Vulkan
drivers or use the GLES fallback:
```bash
RUSTFLAGS="--cfg gles" cargo run
```

## App Bundling
Install `cargo-bundle` from git (the crates.io release has a Windows bug):
```bash
cargo install cargo-bundle --git https://github.com/burtonageo/cargo-bundle
```
Then build the bundle:
```bash
cargo bundle --release
```
On macOS this produces a `.app` bundle, on Windows an `.msi` installer.
Icons are configured in `Cargo.toml` via `package.metadata.bundle.icon`.

## Linux Desktop Integration
Copy the desktop entry and icon to the standard locations:
```bash
cp etc/fileman.desktop ~/.local/share/applications/
cp etc/fileman.svg ~/.local/share/icons/hicolor/scalable/apps/fileman.svg
```

## Contributing
See [CONTRIBUTING.md](CONTRIBUTING.md) for repository layout, testing, and code style.
