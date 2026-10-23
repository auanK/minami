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

The output's parent directory must exist, and `work/` must not exist, even as an empty directory. Success creates exactly `control.json` and `source.json`, following [X 1.0](x-format.md), and leaves stdout empty. Errors go to stderr with a nonzero exit code. The EPUB is never modified. Before either file is staged, an independent audit compares the generated mapping with the lexical XHTML from the same bounded snapshot and rejects missing, duplicated, displaced, or structurally inconsistent content. Translation responses are produced externally.

Extraction translates supported XHTML inline content and retains its original tags, attributes, and nesting during reconstruction. Unknown XHTML elements are rejected.

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

The expected batch must be a complete X envelope whose segments match the regenerated source; only its segment IDs define the required set. Validation regenerates the extraction from the original EPUB and rejects any `control.json` mismatch. A batch run always reports partial work, even when it covers the whole extraction. Success prints `validated N segments (complete extraction)` or `validated N segments (selected batch; not a complete translation)` to stdout; errors go to stderr with a nonzero exit code.

Validation is read-only. It takes a bounded snapshot of the EPUB, regenerates the control and source, compares the supplied control with the regenerated control as a strict JSON tree, and treats only the regenerated map as authority. It checks JSON syntax and schema, global segment coverage, marker identity/multiplicity/topology, global atom order, XHTML ID anchors, promoted NCX boundaries, XML characters, protected spaces, and nonempty translation content. It does not judge translation quality or produce an EPUB. JSON inputs are limited to 64 MiB each, 256 MiB and 10,000 response files in total, 200,000 segments, 1,000,000 parts, 64 MiB of decoded text, 64 levels of JSON or marker nesting, 3,000,000 parsed JSON values, and 3,000,000 array elements or object keys per file. A maximum legal response uses 2,600,004 values and 2,600,003 entries; the parsing budget is charged before each value or entry is constructed. Invalid UTF-8, BOMs, duplicate keys, trailing data, and exceeded limits are rejected without truncation.

To build a translated book from one response:

```bash
minami build book.epub --control work/control.json --translation translated.json --output translated.epub
```

For multiple responses, including externally translated extraction batches:

```bash
minami build book.epub --control work/control.json --translation translated-001.json --translation translated-002.json --translation translated-003.json --output translated.epub
```

`build` performs complete validation internally; a separate `validate` run is optional. Every extracted segment must appear exactly once across all responses, in any file order. Partial batches, duplicate segments, a changed control, or an extraction identity from another edition fail. `build` has no `--expected-batch` option. An extraction without segments still requires an empty X response envelope.

Reconstruction uses the validated owned parts and exact original XHTML buffers. Segments are applied in physical byte order with checked non-overlapping bounds; target text escapes `&`, `<`, `>`, and CR, while tags, atoms, protected spaces, and bytes outside segments come from the original. Each reconstructed document is reparsed and checked for IDs, exact lexical tags and attributes, parent relationships, and NCX fragment targets. Nothing rereads the original EPUB path, control, or responses after preparation: the same private snapshot supplies every other ZIP resource, including unmanifested assets and explicit directory records. OPF, NCX, CSS, images, fonts, and auxiliary files retain their uncompressed bytes. Metadata and attributes are not translated; linguistic meaning is not evaluated.

The output parent must already exist. The destination must not exist as a file, directory, or symlink, including a dangling symlink. Original EPUB, control, and response paths cannot be overwritten. A private sibling staging directory receives the whole ZIP, its final directory is written, and an independent read checks the EPUB and every resource against the snapshot or approved XHTML. Only then does a native no-replace rename publish the file. Concurrent builders have at most one winner. Success leaves stdout empty; failures emit a diagnostic to stderr and a nonzero exit code, with no partial final EPUB.

Both input and output use the supported limits: 128 MiB physical ZIP, 4096 entries, 32 MiB per uncompressed entry/XHTML, 256 MiB total uncompressed bytes, and 8 MiB central directory. Structural XML remains limited to 4 MiB. Output entry sizes and totals are checked before writing; physical size is bounded during writes, header rewrites, and ZIP finalization. JSON, XML, and reconstruction limits above still apply. Oversized translations fail without truncation, splitting, or repair. ZIP64, multipart archives, encryption, symlinks inside ZIP, unsafe/duplicate paths, and unsupported compression remain rejected; ZIP64 local extra fields are also rejected even with a classic end record.

Common build failures include a mismatching `control.json`, missing/duplicate segments, invalid markers or protected spaces, CRC/truncation of an auxiliary resource, output limits, a missing output parent, write/finalization errors, failed staging verification, or an existing destination. Diagnostics retain resource and segment context where available. Temporary data is cleaned up on ordinary errors; abrupt process termination may leave staging data. Atomic visibility does not promise power-loss durability: the builder does not synchronize the file and parent directory to stable storage.

ZIP representation may change. Compression methods, entry names/types, original DOS timestamps when valid, source system/external attributes, raw archive comments, and representable per-entry comment text are preserved. `mimetype` is always physically first, Stored, and has no local extra fields. Other entries follow source central-directory order. Extra fields, data descriptors, original compressed bytes, and comment/name encoding bytes are not preserved; the writer regenerates UTF-8 names/comments, CRCs, sizes, offsets, and headers. Invalid timestamps use the fixed ZIP default (1980-01-01). A legacy entry comment whose decoded UTF-8 representation exceeds 65,535 bytes is omitted. No resource content is omitted. Identical inputs have deterministic logical contents; tests also check byte-identical repeated builds and response-order changes.

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

Tests generate synthetic EPUB fixtures in code. Do not add copyrighted books to the versioned fixtures. The M2.3 CLI integration test runs batched extraction through partial and complete validation, builds in forward and reverse response order, reinspects and reextracts the EPUB, and checks entry coverage and non-XHTML bytes. Other CLI tests cover modified text, hostile ZIP/JSON inputs, rollback, and publication races; internal tests cover snapshot ownership, staging verification, I/O fault injection, and parser budgets. Run these separately with `cargo test --locked --test build_epub`, `cargo test --locked --lib application::build`, and `cargo test --locked --lib epub::packaging`. On Windows, the filesystem symlink destination test is explicitly ignored because it requires symlink privilege or Developer Mode; when available, run `cargo test --locked --test build_epub bld49 -- --ignored`. ZIP symlink rejection runs on all supported platforms without this privilege. The optional reference-book identity test runs only when `MINAMI_REFERENCE_EPUB`, `MINAMI_REFERENCE_CONTROL`, and `MINAMI_REFERENCE_SOURCE` point to an extracted local book, and reads nothing from the working tree.
