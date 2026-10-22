# Minami

Minami is a Rust CLI that prepares EPUB 2 content for external translation and rebuilds books while preserving protected XHTML structure and original resources.

```bash
minami inspect book.epub
minami extract book.epub --output work/
# Translate work/source.json externally into translated.json.
minami validate book.epub --control work/control.json --translation translated.json
minami build book.epub --control work/control.json --translation translated.json --output translated.epub
```

`build` validates the complete translation internally and publishes a verified EPUB to a new path. Repeat `--translation` for multiple response files. Existing destinations are never replaced.

Stack: Rust · Cargo

Development: [docs/development.md](docs/development.md)

Architecture: [docs/architecture.md](docs/architecture.md)
