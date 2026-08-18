# AGENTS.md

Operating manual for coding agents working in this repo. Read
`VISION.md` for why the project exists, `README.md` for architecture
and published numbers.

## Project

`pdfmd` — a Rust crate + CLI that converts PDFs to Markdown with **zero
runtime dependencies**. The whole stack (xref/object-stream reader,
DEFLATE/zlib decoder, font + ToUnicode CMap handling, content-stream
interpreter, heuristics, argv parser) is implemented in this crate.

- Library entry: `src/lib.rs` (`convert_pdf_to_markdown`)
- CLI entry: `src/main.rs` → `src/cli.rs`
- Errors: `pdf::PdfError`, re-exported as `Error`
- Vision: `VISION.md`

## Map

```
src/pdf/          byte-level PDF reader (xref, objects, filters, deflate)
src/extract/      fonts, CMaps, content-stream interpreter, images, layout
src/heuristics/   columns, tables, headings, lists, emphasis → Markdown
src/lib.rs        public API + title promotion + image-mark rewrite
src/cli.rs        argv parser, file/URL/stdin I/O (no clap)
src/util.rs       parallel_map (std::thread::scope worker pool)
src/bin/          profile.rs, opendataloader.rs — dev harnesses, not shipped
tests/            integration + CLI + real fixtures only
```

Public surface stays small: `convert_pdf_to_markdown`, `ConvertOptions`,
`ConvertResult`, `ExtractedImage`, `Error` / `Result`.

## Commands

```sh
cargo build --release                         # LTO release binary
cargo test --all-targets                      # full suite
cargo fmt --all --check                       # CI gate
cargo clippy --all-targets -- -D warnings     # CI gate
RUSTFLAGS="-D warnings" cargo build           # what CI compiles with
cargo llvm-cov --summary-only --ignore-filename-regex 'src/bin/'
cargo run --release --bin profile             # hot-path sanity check
```

Run `fmt`, `clippy`, and `test --all-targets` before pushing. CI runs
that trio on Ubuntu for every PR; macOS and Windows tests run on `main`.

## Hard rules

- **No new dependencies.** `[dependencies]` in `Cargo.toml` must stay
  empty. If a task seems to require one, stop. Pulling in `serde`,
  `clap`, `anyhow`, `flate2`, or a PDF crate defeats the project.
- **MSRV is 1.70** (`rust-version` in `Cargo.toml`). No language or std
  features added after 1.70.
- **Warnings are errors** in CI (`RUSTFLAGS=-D warnings`). Fix them;
  don't `#[allow(...)]` them.
- **Coverage is ~99%.** New code needs tests. Prefer
  `#[cfg(test)] mod tests` next to the code. Use
  `tests/integration.rs` only when a real PDF or the CLI binary is
  required.
- **One error type.** Everything flows through `pdf::PdfError`. Don't
  introduce a second.

## Hot-path discipline

The DEFLATE decoder and the content-stream tokenizer borrow operand
slices out of the source bytes — they do not allocate per operator or
per Huffman code. Fonts are parsed once and cached document-wide. Pages
and multi-page Markdown formatting extract in parallel via
`util::parallel_map` (`std::thread::scope`). Don't regress any of these.

If a change touches `src/pdf/deflate.rs`, `src/extract/parser.rs`,
`src/extract/content/`, or `src/util.rs`, benchmark with
`cargo run --release --bin profile` before calling it done.

Faster without a crate beats cleaner with a crate.

## Conventions

- Comments: terse, explain *why* the code looks unusual, not what it
  does. Don't reference PR numbers, issue IDs, or "added for X".
- Don't add doc comments to private helpers unless there's a real
  subtlety.
- Don't create new top-level files (READMEs, design docs, CHANGELOGs)
  unless asked. `README.md`, `VISION.md`, `AGENTS.md`, and `CLAUDE.md`
  are the first-class docs; keep them in sync when behavior changes.
- Commit subjects use a `module: lowercase summary` prefix —
  e.g. `pdf: cap dictionary entry count to avoid quadratic insert hang`.
- Don't push to `main`; open a PR. Use
  `.github/PULL_REQUEST_TEMPLATE.md`. File issues with the forms in
  `.github/ISSUE_TEMPLATE/` (bug, feature, performance). Do not open
  issues that ask for a runtime crate, encryption, or `LZWDecode`.

## Gotchas

- `*.pdf` is gitignored **except** `tests/fixtures/*.pdf`. New fixtures
  must still match the negated rule.
- URL inputs are fetched by shelling out to `curl` — no HTTP client
  lives in the crate. Tests that hit URLs skip when `curl` is missing.
- `LZWDecode` and encrypted PDFs are unsupported by design. Return
  `PdfError` cleanly; don't add stubs.
- Image XObject extraction is pass-through for JPEG / JPEG 2000, plus
  PNG encoding for decoded 8-bit rasters. The content extractor emits
  `\u{0001}filename\u{0001}` sentinels at paint position;
  `lib::rewrite_image_marks` rewrites them into `![](dir/filename)`.
  Keep both sides in sync.
- The first paragraph is promoted to `# H1` in
  `lib::promote_document_title`, not in `heuristics.rs`. Leave it there.
- `src/bin/profile.rs` and `src/bin/opendataloader.rs` are dev-only,
  excluded from coverage. Don't import them from the library.

## Decision guide

| Situation | Do this |
|-----------|---------|
| Need a crate to parse argv, JSON, HTTP, or PDFs | Stop. Implement the slice you need, or ask. |
| Feature needs encryption or LZW | Return `PdfError`. Do not stub. |
| Unsure where a helper belongs | Put PDF syntax in `src/pdf/`, glyphs/layout in `src/extract/`, Markdown reconstruction in `src/heuristics/`. Shared thread pool stays in `src/util.rs`. |
| Changing reconstruction of titles | Edit `promote_document_title` in `lib.rs`, not heuristics. |
| Only the public API or CLI needs a real file | Add to `tests/integration.rs`. Otherwise unit-test beside the code. |
| Trade-off is speed vs. readability on the hot path | Keep the fast version. Comment *why*. |
