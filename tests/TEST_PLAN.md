# Fileman — Comprehensive Test Plan

## Test Infrastructure

Fileman uses a **replay-based headless testing system**. Tests are RON files in
`tests/cases/` that specify a starting directory, a sequence of keyboard events,
and assertions on panel state, filesystem state, file contents, or screenshots.

Run all tests: `scripts/replay_runner.sh`
Run one test:  `cargo run --release -- --replay tests/cases/<name>.ron`

---

## Feature Coverage Matrix

| # | Feature | Key | Test Case | Status |
|---|---------|-----|-----------|--------|
| 1 | Cursor navigation (Up/Down/Home/End) | Arrow keys | `navigation.ron` | EXISTING |
| 2 | Panel switching (Tab) | Tab | `panels.ron` | EXISTING |
| 3 | Multi-file selection (Insert) | Insert | `selection.ron` | EXISTING |
| 4 | Help screen (F1) | F1 | `help.ron` | EXISTING |
| 5 | File preview (F3) | F3 | `preview.ron` | EXISTING |
| 6 | Preview opens/closes correctly | F3 + Escape | `preview_escape.ron` | EXISTING |
| 7 | File editor (F4) | F4 | `edit.ron` | EXISTING |
| 8 | Create directory (F7) | F7 | `mkdir.ron` | EXISTING |
| 9 | Copy (F5), Rename (Shift+F6), Delete (F8) | F5/Shift+F6/F8 | `file_ops.ron` | EXISTING |
| 10 | Search by filename (Alt+F7) | Alt+F7 | `search.ron` | EXISTING |
| 11 | Search result navigation | Alt+F7 | `search_navigate.ron` | EXISTING |
| 12 | History back/forward (Alt+Left/Right) | Alt+arrows | `history.ron` | EXISTING |
| 13 | Panel swap (Ctrl+U) | Ctrl+U | `panel_swap.ron` | **NEW** |
| 14 | Enter to open directory | Enter | `enter_dir.ron` | **NEW** |
| 15 | Page Up/Down navigation | PgUp/PgDn | `page_navigation.ron` | **NEW** |
| 16 | Move file (F6) | F6 | `move_file.ron` | **NEW** |
| 17 | New file (Shift+F4) | Shift+F4 | `new_file.ron` | **NEW** |
| 18 | Selection toggle (Insert unmark) | Insert×2 | `selection_toggle.ron` | **NEW** |
| 19 | Help close (Escape) | F1 + Escape | `help_close.ron` | **NEW** |
| 20 | Editor save (Ctrl+S) | F4 + Ctrl+S | `edit_save.ron` | **NEW** |
| 21 | Content search (Shift+Alt+F7) | Shift+Alt+F7 | `search_content.ron` | **NEW** |
| 22 | Cross-panel open (Ctrl+Right) | Ctrl+Right | `cross_panel.ron` | **NEW** |

---

## Test Execution Methods

### 1. Headless Replay Tests (primary)
All tests above run via the built-in replay runner — no GPU or display needed.

### 2. GUI Smoke Test (xvfb + xdotool)
Launches the real GUI under Xvfb to verify:
- Application starts and renders a window
- Basic keyboard interaction works (navigate, open help, quit)
- Window responds to xdotool key events

---

## Detailed Test Descriptions

### NEW TEST: panel_swap.ron (#13)
- Navigate down in left panel, Tab to right, navigate down in right panel
- Press Ctrl+U to swap panels
- Assert left panel now has the content/selection that was in right, and vice versa

### NEW TEST: enter_dir.ron (#14)
- Select "out" directory, press Enter to open it
- Assert panel is now inside "out", entries include ".."

### NEW TEST: page_navigation.ron (#15)
- Verify PageDown moves cursor down by a page, PageUp moves back up
- Uses panel state assertions

### NEW TEST: move_file.ron (#16)
- Copy source.txt to out/ first (setup), then move it back
- Assert filesystem state after move

### NEW TEST: new_file.ron (#17)
- Navigate into "out", press Shift+F4, type filename, confirm
- Assert new file exists on filesystem and appears in panel

### NEW TEST: selection_toggle.ron (#18)
- Insert to mark a file, Insert again on same file to unmark
- Assert marked set is empty after toggle

### NEW TEST: help_close.ron (#19)
- Open help with F1, then close with Escape
- Assert both panels return to Browser mode

### NEW TEST: edit_save.ron (#20)
- Open source.txt in editor (F4), type text, save with Ctrl+S
- Assert file content on disk matches edited version

### NEW TEST: search_content.ron (#21)
- Open content search (Shift+Alt+F7), search for text in source.txt
- Assert search results contain the file

### NEW TEST: cross_panel.ron (#22)
- From left panel, select "out", press Ctrl+Right
- Assert right panel navigated into "out" directory
