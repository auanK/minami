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

The output's parent directory must exist, and `work/` must not exist, even as an empty directory. Success creates exactly `control.json` and `source.json`, following [X 1.0](x-format.md), and leaves stdout empty. Errors go to stderr with a nonzero exit code. The EPUB is never modified. Before either file is staged, an independent audit compares the generated mapping with the lexical XHTML from the same bounded snapshot and rejects missing, duplicated, displaced, or structurally inconsistent content. Translation responses are produced externally. An internal reconstruction engine exists for the next milestone, but there is no reconstruction command and no EPUB packaging yet.

To split the same extraction into section-scoped translation batches:

```bash
cargo run --locked -- extract book.epub --output work/ --batch-max-bytes 32768
```

With this option, success creates `control.json` and `batches/` instead of `source.json`. Each file under `batches/` is a complete X 1.0 envelope containing only segments from one section, named `qNNNNNN-NNNNNN.json`. `control.json` is byte-for-byte identical to monolithic extraction. The limit applies to the complete serialized UTF-8 JSON file, including envelope fields, indentation, escapes, separators, and the final newline; `32768` is a recommended starting point. A segment is never split, and batches never cross section boundaries. An individual segment that cannot fit fails explicitly with its segment and section IDs, and nothing is published. A section without segments produces no file; an extraction without segments still publishes an empty `batches/` directory. The same independent audit and atomic no-replace publication are used. The limit must be positive and no larger than the 64 MiB serialization cap.

To validate externally produced responses against the original EPUB:

```bash
cargo run --locked -- validate book.epub --control work/control.json --translation response-1.json --translation response-2.json
```

Without `--expected-batch`, the responses must together cover every extracted segment exactly once. `--translation` may be repeated, and file order is irrelevant. For one generated batch, pass exactly one response and its batch file:

```bash
cargo run --locked -- validate book.epub --control work/control.json --translation response.json --expected-batch work/batches/q000001-000062.json
```

The expected batch must be a complete X envelope whose segments match the regenerated source; only its segment IDs define the required set. A batch run always reports partial work, even when it covers the whole extraction. Success prints `validated N segments (complete extraction)` or `validated N segments (selected batch; not a complete translation)` to stdout; errors go to stderr with a nonzero exit code.

Validation is read-only. It takes a bounded snapshot of the EPUB, regenerates the control and source, compares the supplied control with the regenerated control as a strict JSON tree, and treats only the regenerated map as authority. It checks JSON syntax and schema, global segment coverage, marker identity/multiplicity/topology, global atom order, XHTML ID anchors, promoted NCX boundaries, XML characters, protected spaces, and nonempty translation content. It does not judge translation quality or produce an EPUB. JSON inputs are limited to 64 MiB each, 256 MiB and 10,000 response files in total, 200,000 segments, 1,000,000 parts, 64 MiB of decoded text, 64 levels of JSON or marker nesting, 3,000,000 parsed JSON values, and 3,000,000 array elements or object keys per file. A maximum legal response uses 2,600,004 values and 2,600,003 entries; the parsing budget is charged before each value or entry is constructed. Invalid UTF-8, BOMs, duplicate keys, trailing data, and exceeded limits are rejected without truncation.

The internal reconstruction path is not reachable from the CLI. It accepts only a complete validated set, not a partial expected batch, and keeps the validated parts plus the exact XHTML bytes read from the authorized extraction snapshot. It does not reread the EPUB, control, or response after validation. Segments are applied in physical byte order with checked non-overlapping bounds; target text escapes `&`, `<`, `>`, and CR, while tags, atoms, protected spaces, and bytes outside segments come from the original. Every reconstructed document is reparsed and checked for IDs, exact lexical tags and attributes, parent relationships, and NCX fragment targets before the in-memory result is returned. Each result document is capped at 32 MiB, checked with the input limits above and before allocation; a failure returns no partial list and writes no files. `minami build`, EPUB packaging, and non-XHTML resource rewriting belong to the next milestone.

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

Tests generate small EPUB fixtures in code. Do not add copyrighted books to the versioned fixtures. The optional reference-book identity test runs only when `MINAMI_REFERENCE_EPUB`, `MINAMI_REFERENCE_CONTROL`, and `MINAMI_REFERENCE_SOURCE` point to an extracted local book, and reads nothing from the working tree.
