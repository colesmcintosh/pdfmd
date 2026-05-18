# AGENTS.md

Guidance for coding agents (Claude Code, Codex, Copilot, etc.) working in
this repo. See `README.md` for the architecture diagram, performance
numbers, and user-facing usage.

## Project

`pdfmd` — a Rust crate + CLI that converts PDFs to Markdown with zero
runtime dependencies. The whole stack (xref/object-stream reader,
DEFLATE/zlib decoder, font + ToUnicode CMap handling, content-stream
interpreter, heuristics, argv parser) is implemented in this crate.

- Library entry: `src/lib.rs` (`convert_pdf_to_markdown`)
- CLI entry: `src/main.rs`
- Errors: `pdf::PdfError`, re-exported as `Error`

## Commands

```sh
cargo build --release                         # LTO release binary
cargo test --all-targets                      # 326 tests
cargo fmt --all --check                       # CI gate
cargo clippy --all-targets -- -D warnings     # CI gate
RUSTFLAGS="-D warnings" cargo build           # what CI compiles with
cargo llvm-cov --summary-only --ignore-filename-regex 'src/bin/'
```

Run `fmt`, `clippy`, and `test --all-targets` before pushing. CI runs
the same trio on Linux, macOS, and Windows.

## Hard rules

- **No new dependencies.** `[dependencies]` in `Cargo.toml` must stay
  empty. If a task seems to require one, stop and ask. Pulling in
  `serde`, `clap`, `anyhow`, or a PDF crate defeats the project.
- **MSRV is 1.70** (`rust-version` in `Cargo.toml`). No language or std
  features added after 1.70.
- **Warnings are errors** in CI (`RUSTFLAGS=-D warnings`). Fix them;
  don't `#[allow(...)]` them.
- **Coverage is ~99.75%.** New code needs tests. Prefer
  `#[cfg(test)] mod tests` next to the code. Use
  `tests/integration.rs` only when a real PDF or the CLI binary is
  required.
- **One error type.** Everything flows through `pdf::PdfError`. Don't
  introduce a second.

## Hot-path discipline

The DEFLATE decoder and the content-stream tokenizer borrow operand
slices out of the source bytes — they do not allocate per operator or
per Huffman code. Fonts are parsed once and cached document-wide. Pages
extract in parallel via `std::thread::scope`. Don't regress any of
these — benchmark with `cargo run --release --bin profile` if you're
unsure.

## Conventions

- Comments: terse, explain *why* the code looks unusual, not what it
  does. Don't reference PR numbers, issue IDs, or "added for X".
- Don't add doc comments to private helpers unless there's a real
  subtlety.
- Don't create new top-level files (READMEs, design docs, CHANGELOGs)
  unless asked.
- Commit subjects use a `module: lowercase summary` prefix —
  e.g. `pdf: cap dictionary entry count to avoid quadratic insert hang`.
- Don't push to `main`; open a PR.

## Gotchas

- `*.pdf` is gitignored **except** `tests/fixtures/*.pdf`. New fixtures
  must still match the negated rule.
- URL inputs are fetched by shelling out to `curl` — no HTTP client
  lives in the crate. Tests that hit URLs skip when `curl` is missing.
- `LZWDecode` and encrypted PDFs are unsupported by design. Return
  `PdfError` cleanly; don't add stubs.
- Image XObject extraction is pass-through only (JPEG, JPEG 2000). The
  content extractor emits `\u{0001}filename\u{0001}` sentinels at paint
  position; `lib::rewrite_image_marks` rewrites them into
  `![](dir/filename)`. Keep both sides in sync.
- The first paragraph is promoted to `# H1` in
  `lib::promote_document_title`, not in `heuristics.rs`. Leave it there.
- `src/bin/profile.rs` is a dev-only profiling harness, excluded from
  coverage. Don't import it from the library.
