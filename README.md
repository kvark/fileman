# FileMan

FileMan is a fast, responsive two-panel file manager built with Rust, egui, and blade-egui. Navigation stays snappy even in large directories by doing all I/O off the UI thread and streaming results into the view.

![FileMan screenshot](etc/snapshots/tests/preview.png)

## Features
- **Dual-panel layout** with independent navigation, history (Alt+Left/Right), panel swap (Ctrl+U), and tab support.
- **Async I/O** — directory loading streams in batches; all I/O runs off the UI thread so navigation never stalls.
- **SFTP remote browsing** — connect to any SSH host (reads `~/.ssh/config`), navigate and operate on remote files as naturally as local ones.
- **Remote search** (Alt+F7 on a remote panel) — runs `find` or `grep` over SSH; results stream back and open directly.
- **Archive navigation** for zip, tar, tar.gz, and tar.bz2 — browse like regular folders, copy files out, or open with system apps.
- **Preview** (F3): text with syntax highlighting, images (JPEG, PNG, GIF, WebP, BMP, TGA, HDR, DDS) including animated GIF, and archive listings.
- **Inline editor** (F4) with syntax highlighting; create new files with Shift+F4.
- **File operations**: copy (F5), move (F6), delete (F8), rename (Shift+F6), new directory (F7) — all work on local and remote panels, with a progress bar for large transfers.
- **Search** (Alt+F7) by name or content, with wildcard and case-insensitive options; results displayed as a virtual folder you can navigate and operate on.
- **Theming**: external theme files in `themes/` (JSON, YAML, or TOML), toggle with F9, pick with F10.

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
| Ctrl+G | Quick jump |
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
Install the binary, desktop entry, and icon to `~/.local` (the standard per-user prefix):
```bash
make install
```
To install system-wide instead:
```bash
sudo make install PREFIX=/usr
```
To remove:
```bash
make uninstall
```

## Contributing
See [CONTRIBUTING.md](CONTRIBUTING.md) for repository layout, testing, and code style.
