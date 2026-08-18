# CLAUDE.md

Claude-specific entry for this repo. Read this first, then
`VISION.md` (why) and `AGENTS.md` (rules, map, gotchas).

## What you are working on

`pdfmd` converts PDF bytes to Markdown. It is meant to be the fastest
converter that does not depend on external crates. The PDF reader,
DEFLATE decoder, font/CMap stack, content-stream interpreter, layout
heuristics, and CLI parser all live in this crate.

`[dependencies]` is empty on purpose. Do not add a crate to save time.

## How to work here

1. Read `VISION.md` if the change touches goals, scope, or dependencies.
2. Read `AGENTS.md` before editing. Follow its hard rules and gotchas.
3. Change the smallest surface that fixes the task.
4. Put tests next to the code (`#[cfg(test)] mod tests`). Use
   `tests/integration.rs` only for a real PDF or the CLI binary.
5. Verify with the commands below before you consider the work done.

Do not introduce a second error type. Do not add post-1.70 Rust
features. Do not `#[allow]` warnings that CI will reject.

## Verify

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

If you touched `src/pdf/deflate.rs`, `src/extract/parser.rs`,
`src/extract/content/`, or `src/util.rs`, also run:

```sh
cargo run --release --bin profile
```

If you touched `src/ffi.rs` or `python/`, also run:

```sh
cargo build --release
python -m unittest discover -s python/tests
```

## Scope

- Own the stack; don't wrap someone else's.
- Encrypted PDFs and `LZWDecode` stay unsupported — return `PdfError`.
- Title promotion lives in `lib::promote_document_title`, not heuristics.
- Image sentinels (`\u{0001}filename\u{0001}`) and
  `lib::rewrite_image_marks` must stay in sync.
- Bindings go through the C ABI in `src/ffi.rs`. No PyO3, no maturin —
  `python/` is `ctypes` and stdlib only, on both sides.
- `src/bin/*` is profiling / comparison only. Don't import it.

Commit subjects: `module: lowercase summary`. Don't push to `main`.
Fill `.github/PULL_REQUEST_TEMPLATE.md` on every PR. New issues use
the forms in `.github/ISSUE_TEMPLATE/` — pick bug, feature, or
performance, and refuse work that needs a crate, encryption, or LZW.
