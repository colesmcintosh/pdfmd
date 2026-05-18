# Agent guide

Context and conventions for AI coding agents (Claude Code, etc.) working in
this repo. Human contributors should also find it useful.

## What this is

`pdfmd` is a zero-dependency PDF → Markdown converter, written in Rust.
Every layer — argv parser, error type, work scheduler, PDF reader,
DEFLATE/zlib decoder, font and CMap handling, content-stream interpreter,
heuristics — lives in this crate. `Cargo.toml` has an empty
`[dependencies]` table; keep it that way.

Public surface is small:

- `convert_pdf_to_markdown(bytes, &ConvertOptions) -> Result<ConvertResult>`
- `ConvertOptions { include_page_breaks, image_dir }`
- `ConvertResult { markdown, images }`
- `ExtractedImage` (re-exported from `extract`)

The CLI in `src/main.rs` is a thin wrapper around that function.

## Layout

```
src/
├── pdf/           PDF byte-level reader
│   ├── deflate.rs   zlib + DEFLATE decoder (RFC 1950 / 1951)
│   ├── object.rs    Object / Dictionary / Stream model
│   ├── parser.rs    object parser
│   └── mod.rs       Document, xref, object streams, /FlateDecode chain
├── extract/       text + image extraction off the parsed PDF
│   ├── encoding.rs  WinAnsi / MacRoman / Standard / Symbol byte → glyph
│   ├── glyphs.rs    glyph name → Unicode, with uniXXXX fallback
│   ├── cmap.rs      ToUnicode CMap parser (bfchar / bfrange)
│   ├── font.rs      per-font byte → text decoder
│   ├── parser.rs    streaming content-stream tokenizer
│   ├── content.rs   content-stream interpreter + text-state machine
│   ├── image.rs     pass-through JPEG / JPEG 2000 extraction
│   └── mod.rs       page walking + per-page parallelism
├── heuristics.rs    headings, lists, paragraph reflow
├── lib.rs           public API (assembly + post-processing)
├── main.rs          CLI binary
└── bin/profile.rs   developer-only profiling harness (excluded from coverage)
tests/
├── fixtures/        sample PDFs used by integration tests
└── integration.rs   end-to-end library + CLI tests
```

Data flow: `extract::extract_text` produces a `Vec<String>` (one per page)
plus any extracted images, `heuristics::format_page` reflows each page's
text into markdown, then `lib.rs` joins, rewrites image sentinels, and
promotes the first paragraph to an H1.

## Build, test, lint

```sh
cargo build                   # debug
cargo build --release         # release (LTO, single codegen unit)
cargo test --all-targets      # 326 tests; CI also runs this on macOS/Windows
cargo fmt --all --check       # rustfmt
cargo clippy --all-targets -- -D warnings
```

CI (`.github/workflows/ci.yml`) compiles with `RUSTFLAGS=-D warnings` on
ubuntu / macOS / windows. Treat any warning as a failure locally too.

Coverage:

```sh
cargo install cargo-llvm-cov
cargo llvm-cov --summary-only --ignore-filename-regex 'src/bin/'
```

The `src/bin/` exclusion drops `profile.rs`, which is a dev tool, not part
of the shipped library or CLI.

## Conventions agents must follow

- **No dependencies.** Do not add anything to `[dependencies]`. If a task
  seems to require one, stop and ask. Adding `serde`, `regex`, `clap`, a
  PDF crate, etc. defeats the point of the project.
- **MSRV is 1.70** (`rust-version` in `Cargo.toml`). Don't use language or
  std features added after that.
- **Coverage is ~99.75% and CI enforces formatting/clippy.** New code
  needs tests. Prefer small unit tests next to the code (`#[cfg(test)]
  mod tests` at the bottom of the file) and reserve `tests/integration.rs`
  for things that need a real PDF on disk or shell out to the CLI binary.
- **Hot path discipline.** The tokenizer and DEFLATE decoder deliberately
  borrow operand slices out of the source bytes — don't introduce
  per-operator or per-Huffman-code allocations. Fonts are parsed once and
  shared across pages via a document-wide cache; per-page work runs on a
  `std::thread::scope` worker pool. Don't regress either of those.
- **Errors flow through `pdf::PdfError`** (re-exported as `Error`). Don't
  introduce a second error type or pull in `thiserror`/`anyhow`.
- **Image sentinels.** The content extractor emits `\u{0001}filename\u{0001}`
  at each image's paint position; `lib::rewrite_image_marks` rewrites
  those into `![](dir/filename)`. If you touch either side, keep them in
  sync and add a test.
- **Comments.** Match the existing style: terse, explain *why* the code
  looks unusual, not what it does. Don't add doc comments to private
  helpers unless there's a real subtlety. Don't reference task numbers,
  PRs, or "added for X" in code — that belongs in commit messages.
- **No new top-level files** (READMEs, design docs, changelogs) unless
  asked. The repo is intentionally small.

## Things that will surprise you

- `*.pdf` is gitignored except for `tests/fixtures/*.pdf`. If you add a
  fixture, make sure the negated rule still matches.
- URL inputs are fetched by shelling out to `curl` — there's no HTTP
  client in the crate. Tests that exercise this skip when `curl` is
  absent.
- `LZWDecode` streams and encrypted PDFs are unsupported by design.
  Don't add stubs that pretend otherwise; return `PdfError` cleanly.
- The first paragraph of the output is promoted to `# H1` after
  heuristics run, because the generic heading rules can't tell a title
  from prose. Keep this in `lib::promote_document_title`, not in
  `heuristics.rs`.

## When you're done

- `cargo fmt --all`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --all-targets`

All three must pass before pushing. If coverage drops noticeably on a
changed file, add tests rather than papering over with `#[cfg_attr]`.
