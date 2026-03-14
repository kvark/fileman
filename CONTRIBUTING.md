# Contributing

## Workflow

Ensure `cargo fmt` is ran and `cargo clippy` is clean.

## Code Style Guide

- keep dependencies and amount of code low
- simple is good, don't overcomplicate or anticipate
- one `use` per crate, prefer importing modules instead of concrete types/functions
  - ok to include individual members if there is a few of them. As soon as it doesn't fit into a line - switch to using more complete paths, e.g. `fs::File` instead of `File`.
- don't rely on implicit references via `match`, always use explicit `ref` instead
- use enums instead of boolean arguments

## Repository Layout

- `src/main.rs` — app entry point, event loop, directory loading
- `src/archive.rs` — container plugins (zip, tar, tar.gz, tar.bz2)
- `src/core.rs` — shared types and utilities
- `src/app_state.rs` — application state
- `src/input.rs` — keyboard handling
- `src/ui/` — UI components (panel, preview, help)
- `src/image_decode.rs` — image decoding (including animated GIF)
- `src/replay.rs` — replay case data structures and assertion types
- `src/replay_runner.rs` — headless replay executor and assertion logic
- `themes/` — external theme files
- `etc/` — desktop entry, icon, reference snapshots
- `tests/cases/` — replay test cases (RON format)
- `tests/data/` — test fixture data
- `scripts/replay_runner.sh` — runs all replay cases with per-test cleanup

## Testing

Tests use a replay system that drives the application headlessly. Each test case
is a RON file in `tests/cases/` that specifies a starting directory, a sequence
of key events, and assertions to check after execution.

Run all replay cases:
```bash
scripts/replay_runner.sh
```

Run a single case:
```bash
cargo run --release -- --replay tests/cases/search.ron
```

Emit a screenshot while replaying:
```bash
cargo run --release -- --replay tests/cases/preview.ron --snapshot /tmp/replay.png
```

### Replay case format

```ron
(
  root: "tests/data/basic",       // starting directory (both panels)
  left: Some("path/to/left"),     // optional: override left panel root
  right: Some("path/to/right"),   // optional: override right panel root
  state_dump: Some("target/test-artifacts/dump.ron"),  // optional: write state to file
  keys: [
    (key: "Wait"),                // wait for async loading to finish
    (key: "Down"),                // bare key press
    (key: "F7", modifiers: ["Alt"]),  // key with modifiers
    (key: "text:hello"),          // inject text input
    (key: "select:source.txt"),   // move cursor to named entry
    (key: "replace:new_name"),    // set inline rename text
    (key: "wait:500"),            // wait for a fixed duration (ms)
  ],
  asserts: (
    // all assertion fields are optional
  ),
)
```

### Assertion types

There are three kinds of assertions, and they can be combined in a single test.

#### 1. Filesystem checks

Verify that files and directories exist (or match exactly) on disk after the
replay. Useful for testing operations like copy, move, delete, and mkdir.

```ron
asserts: (
  // Check directory tree on disk
  fs: Some((
    mode: Exact,    // or Contains
    entries: [
      (path: "out", kind: Dir),
      (path: "source.txt", kind: File),
    ],
  )),
  // Check file contents
  files: [
    (path: "out/copy.txt", contains: Some("expected text")),
    (path: "out/exact.txt", equals: Some("full content")),
  ],
),
```

`mode: Exact` fails if there are any entries on disk not listed in the assertion.
`mode: Contains` only checks that the listed entries are present.

#### 2. Screenshot comparison

Render the UI to a PNG and compare it against a reference image with configurable
tolerance. Useful for visual regression testing of the UI layout.

```ron
asserts: (
  snapshots: [
    (
      path: "target/test-artifacts/preview.png",
      expected: "etc/snapshots/tests/preview.png",
      max_channel_diff: 200,      // per-channel tolerance (0–255)
      max_pixel_fraction: 0.003,  // fraction of pixels allowed to differ
    ),
  ],
),
```

To update reference images after intentional UI changes:
```bash
cp target/test-artifacts/*.png etc/snapshots/tests/
```

#### 3. Panel state checks

Inspect the internal state of each panel: entry list, selected entry, mode, and
marked set. This is the fastest way to test navigation, search, selection, and
mode transitions without relying on pixel output.

```ron
asserts: (
  left_panel: Some((
    mode: Exact,                        // or Contains; applies to entries list
    entries: ["..", "out", "file.txt"],  // expected entry names
    selected: Some("file.txt"),         // cursor position
    browser_mode: Some("Fs"),           // Fs, Container, or Search
    panel_mode: Some("Browser"),        // Browser, Preview, Edit, or Help
    marked: ["out", "file.txt"],        // multi-selected entries
  )),
  right_panel: Some(( /* same fields */ )),
),
```

All fields inside a panel assert are optional — omit any you don't care about.

### State dumps

For debugging or developing new tests, request a full state dump:

```ron
(
  root: "tests/data/basic",
  state_dump: Some("target/test-artifacts/debug-state.ron"),
  keys: [ (key: "Wait") ],
  asserts: (),
)
```

This writes a RON file with both panels' entries, cursor positions, modes, and
sort settings. Inspect it to determine the right assertion values for a new test.
