# Minami

Minami is a Rust CLI for EPUB workflows that keep translation external. Its long-term goal is to prepare book content for external LLM translation and rebuild EPUBs while preserving their structure.

Currently, Minami supports structural inspection of EPUB 2 books, extraction of XHTML content to X 1.0 for external translation, and structural validation of translated X 1.0 responses against a freshly regenerated extraction. It does not yet rebuild EPUBs.

Stack: Rust · Cargo

Development: [docs/development.md](docs/development.md)

Architecture: [docs/architecture.md](docs/architecture.md)
