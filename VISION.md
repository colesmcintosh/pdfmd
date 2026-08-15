# Vision

`pdfmd` is the fastest PDF → Markdown converter that depends on nothing
outside the Rust standard library.

Speed is the product. Zero dependencies is how we keep it that way.

## North star

Own every byte of the path from PDF input to Markdown output. Parse the
object graph, inflate streams, decode fonts, interpret content operators,
and reconstruct layout — all in this crate, with no HTTP client, no PDF
library, no CLI framework, and no serde.

A typical academic paper should convert in a few milliseconds. A
hundred-megabyte document should still feel instant. The release binary
should stay small enough to copy onto a machine that has only a Rust
toolchain, or none at all.

## Why zero dependencies

External crates are a tax: version conflicts, supply-chain review,
compile-time bloat, and behavior you cannot tune. A PDF crate would own
the hot path and leave us stuck on its allocations and API. `clap` or
`anyhow` would save a few dozen lines and cost the project's identity.

Implementing the stack here is less headache and less reliance on
external software. It is also the only way to keep the tokenizer and
DEFLATE decoder borrowing operand slices instead of allocating per
operator or Huffman code.

`[dependencies]` in `Cargo.toml` stays empty. That is not a preference.
It is the project.

## What "fastest" means

- End-to-end wall time from bytes to Markdown, not a microbenchmark of
  one stage.
- Throughput that scales with page count: fonts parsed once, pages and
  Markdown formatting fanned out with `std::thread::scope`.
- No per-operator or per-code allocations on the content-stream or
  DEFLATE hot paths.
- A ~700 KB LTO release binary. No runtime to ship, no shared libraries
  to find.

If a change makes the code prettier but slower, it is the wrong change.
If a change needs a crate to go faster, it is the wrong change.

## Principles

1. **Own the stack.** Xref, object streams, filters, fonts, CMaps,
   content streams, heuristics, argv parsing — all here.
2. **Borrow, don't clone.** Hot-path parsers take `&[u8]` and return
   slices or small values. Allocate at document or page boundaries, not
   per token.
3. **One error type.** `pdf::PdfError`, re-exported as `Error`. Callers
   get a single `Result`.
4. **Fail cleanly on non-goals.** Encrypted PDFs and `LZWDecode` return
   `PdfError`. No stubs, no half-implemented crypto.
5. **Heuristics stay honest.** Column order, tables, headings, and lists
   are best-effort reconstruction. Do not pretend PDFs are semantic.
6. **Coverage is a feature.** New code ships with tests next to it.
   Integration tests exist only when a real PDF or the CLI binary is
   required.

## What we will do

- Make conversion faster without growing the dependency set.
- Recover more text and better Markdown from academic and prose PDFs
  (columns, GFM tables, headings, lists, emphasis, hyphenation).
- Keep the public API small: `convert_pdf_to_markdown`, `ConvertOptions`,
  `ConvertResult`, `Error`.
- Stay on MSRV 1.70 so the crate builds on older toolchains.

## What we will not do

- Add runtime crates (`serde`, `clap`, `anyhow`, `lopdf`, `flate2`,
  HTTP clients, async runtimes).
- Become a general PDF toolkit, renderer, or editor.
- Support encryption or `LZWDecode`.
- Chase pixel-perfect layout for forms, invoices, or complex math.
- Grow a plugin system, config format, or markdown dialect of our own.

## Success

`pdfmd` is the default answer when someone wants Markdown from a PDF
and does not want to pull in a stack. It is faster than the published
numbers of heavier libraries, small enough to vendor, and simple enough
that an agent or a human can change the hot path without learning five
other crates first.
