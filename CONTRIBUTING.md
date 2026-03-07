# Workflow

Ensure `cargo fmt` is ran and `cargo clippy` is clean.

# Code Style Guide

- keep dependencies and amount of code low
- simple is good, don't overcomplicate or anticipate
- one `use` per crate, prefer importing modules instead of concrete types/functions
  - ok to include individual members if there is a few of them. As soon as it doesn't fit into a line - switch to using more complete paths, e.g. `fs::File` instead of `File`.
- don't rely on implicit references via `match`, always use explicit `ref` instead
- use enums instead of boolean arguments
