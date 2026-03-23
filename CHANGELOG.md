# Changelog

## 0.2.1 (TBD)

### Features
- Remote browsing via built-in SFTP
- New syntax: Makefile, Dockerfile, CSV, SVG
- Defaulting to Ctrl+? schema on Apple devices
- Quick navigation menu with Ctrl+G

### Fixes
- Fix archive navigation

## 0.2.0 (19 Mar 2026)

### Features
- Support GLES for older systems (separate build)
- Tab support (same keys as browsers)
- Self-update: `fileman --update` checks GitHub releases and replaces the binary in-place (compile feature `self-update`, enabled for tarball/zip/AppImage/MSI, disabled for deb/rpm)
- Introspection about async tasks displayed in F1 help screen
- Multi-stage JPEG loading for instant views
- New image formats: TGA, HDR, and DDS
- New syntax: RON

## 0.1.0 (13 Mar 2026)

Initial release.

### Features
- Two-panel file manager with keyboard-driven navigation
- Archive browsing: zip, tar, tar.gz, tar.bz2 (read-only, inline navigation)
- File operations: copy (F5), move (F6), rename (Shift+F6), delete (F8), mkdir (F7), new file (Shift+F4)
- Multiple selection with Insert key
- Integrated text editor (F4) with syntax highlighting
- File preview (F3): text, hex, images (JPEG, PNG, BMP, GIF, WebP), EXIF metadata
- Animated GIF playback in preview
- Search by name or content (Alt+F7, Shift+Alt+F7) with wildcard support
- File properties dialog (Alt+Enter) on Unix
- Open files with default system application (Shift+Enter)
- Symlink display with target paths
- Directory size calculation (Space)
- Navigation history (Alt+Left, Alt+Right)
- Configurable sort by name, size, or date
- Dark/light themes with external theme support (F9, F10)
- Help screen (F1)
- Replay-based testing framework with snapshot assertions
