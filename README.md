# Minami

Minami is an offline Rust CLI and library for inspecting, extracting, validating, and rebuilding EPUB 2 books using the X 1.0 XHTML rules. It prepares translation files for external tools and checks their structure before rebuilding the book.

Minami does not translate text automatically, support EPUB 3, translate metadata or attributes, or guarantee translation quality. It rejects XHTML and ZIP features outside its documented supported subset; it does not promise compatibility with every EPUB.

Install Rust 1.88 or newer, then build with:

```bash
cargo build --release --locked
```

Complete workflow:

```bash
minami inspect book.epub
minami extract book.epub --output work/
# Translate work/source.json externally into translated.json.
minami validate book.epub --control work/control.json --translation translated.json
minami build book.epub --control work/control.json --translation translated.json --output translated.epub
```

Extraction translates supported XHTML inline content and preserves its original markup and attributes during reconstruction. Unknown XHTML elements are rejected.

For section batches and detailed usage, limits, and supported features, see [docs/development.md](docs/development.md). The [architecture](docs/architecture.md) and [X 1.0 contract](docs/x-format.md) describe the implementation and file format.
