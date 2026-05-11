# rust-parse

A fast, dependency-light PDF → Markdown converter written in Rust.

`rust-parse` extracts text directly from a PDF — walking the object graph,
decoding fonts, and interpreting the content-stream operators itself — and
then runs a small set of heuristics to recover headings, lists, and
paragraph boundaries.

## Why

PDFs do not carry semantic structure. Most extraction libraries either
return one undifferentiated text blob, or stop at the slow, general-purpose
parsing layer. `rust-parse` skips both: it owns the path from bytes to
Markdown, which keeps the conversion under ~15 ms for a typical
academic paper and leaves room to tune the heuristics for the documents
you care about.

## Install

```sh
cargo install --path .
```

Or build a release binary directly:

```sh
cargo build --release
# binary is at ./target/release/rust-parse
```

## Usage

```sh
rust-parse input.pdf                   # markdown to stdout
rust-parse input.pdf -o output.md      # write to a file
rust-parse input.pdf --page-breaks     # insert `---` between PDF pages
cat input.pdf | rust-parse -           # read from stdin
```

## Performance

Benchmark on a 1.05 MB, 17-page arXiv paper (Apple Silicon, release build,
20 runs, warm cache):

| metric     | value                |
|------------|----------------------|
| min        | 11.5 ms              |
| median     | 12.4 ms              |
| mean       | 12.3 ms ± 0.4 ms     |
| throughput | ~85 MB/s of PDF      |
| pages/sec  | ~1,400               |

## How it works

```
src/
├── extract/
│   ├── encoding.rs    WinAnsi / MacRoman / Standard / Symbol byte → glyph
│   ├── glyphs.rs      glyph name → Unicode, with uniXXXX fallback
│   ├── cmap.rs        ToUnicode CMap parser (bfchar / bfrange)
│   ├── font.rs        per-font byte → text decoder
│   ├── content.rs     content-stream interpreter and text-state machine
│   └── mod.rs         page walking
├── heuristics.rs      headings, lists, paragraph reflow
├── lib.rs             public API
└── main.rs            CLI
```

The only PDF-specific dependency is [`lopdf`](https://crates.io/crates/lopdf)
for parsing the object graph; everything above that — fonts, encodings,
ToUnicode CMaps, content streams, layout heuristics — is implemented in
this crate.

## Limitations

- Tables, multi-column figure captions, and complex math layout come out
  as best-effort reflowed text.
- Fonts that ship without a `ToUnicode` CMap and use neither a standard
  encoding nor a `/Differences` array will silently drop glyphs.
- The heuristic layer targets academic and prose documents. Forms,
  invoices, and other heavily-structured PDFs will not reconstruct well.

## License

MIT.
