# Development

## Requirements

Rust 1.88 or newer, with the 2024 edition, and Cargo. Install the Rust toolchain using [rustup](https://www.rust-lang.org/tools/install).

## Build

```bash
cargo build --locked
```

## Run

```bash
cargo run --locked -- inspect book.epub
```

The command writes a JSON report to stdout on success and an error to stderr on failure.

## Formatting

```bash
cargo fmt --all -- --check
```

## Lint

```bash
cargo clippy --all-targets -- -D warnings
```

## Tests

```bash
cargo test --locked
```

Tests generate small EPUB fixtures in code. Do not add copyrighted books to the versioned fixtures.
