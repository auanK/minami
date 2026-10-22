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

The output's parent directory must exist, and `work/` must not exist, even as an empty directory. Success creates exactly `control.json` and `source.json`, following [X 1.0](x-format.md), and leaves stdout empty. Errors go to stderr with a nonzero exit code. The EPUB is never modified. Before either file is staged, an independent audit compares the generated mapping with the lexical XHTML from the same bounded snapshot and rejects missing, duplicated, displaced, or structurally inconsistent content. Translation responses are produced externally; translation validation and EPUB reconstruction are not implemented.

To split the same extraction into section-scoped translation batches:

```bash
cargo run --locked -- extract book.epub --output work/ --batch-max-bytes 32768
```

With this option, success creates `control.json` and `batches/` instead of `source.json`. Each file under `batches/` is a complete X 1.0 envelope containing only segments from one section, named `qNNNNNN-NNNNNN.json`. `control.json` is byte-for-byte identical to monolithic extraction. The limit applies to the complete serialized UTF-8 JSON file, including envelope fields, indentation, escapes, separators, and the final newline; `32768` is a recommended starting point. A segment is never split, and batches never cross section boundaries. An individual segment that cannot fit fails explicitly with its segment and section IDs, and nothing is published. A section without segments produces no file; an extraction without segments still publishes an empty `batches/` directory. The same independent audit and atomic no-replace publication are used. The limit must be positive and no larger than the 64 MiB serialization cap.

Extraction uses a bounded private temporary snapshot and prepares all JSON files or batches before publishing their directory. Atomic publication without replacing an existing destination is supported on Windows and Linux/Android; other platforms fail explicitly. Temporary files are cleaned up on ordinary errors; process termination or power loss may leave a staging directory, and power-loss durability is not promised.

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
