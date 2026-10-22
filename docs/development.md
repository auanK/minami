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

To extract translation input:

```bash
cargo run --locked -- extract book.epub --output work/
```

The output's parent directory must exist, and `work/` must not exist, even as an empty directory. Success creates exactly `control.json` and `source.json`, following [X 1.0](x-format.md), and leaves stdout empty. Errors go to stderr with a nonzero exit code. The EPUB is never modified. Translation responses are produced externally; translation validation and EPUB reconstruction are not implemented.

Extraction uses a bounded private temporary snapshot and prepares both JSON files before publishing their directory. Atomic publication without replacing an existing destination is supported on Windows and Linux/Android; other platforms fail explicitly. Temporary files are cleaned up on ordinary errors; process termination or power loss may leave a staging directory, and power-loss durability is not promised.

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
