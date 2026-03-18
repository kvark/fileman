# Changelog

## Unreleased

### Features
- Self-update: `fileman --update` checks GitHub releases and replaces the binary in-place (compile feature `self-update`, enabled for tarball/zip/AppImage/MSI, disabled for deb/rpm)

## 0.1.0

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
