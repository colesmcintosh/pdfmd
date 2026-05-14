# pdfmd

A fast, dependency-light PDF → Markdown converter written in Rust.

`pdfmd` extracts text directly from a PDF — walking the object graph,
decoding fonts, and interpreting the content-stream operators itself — and
then runs a small set of heuristics to recover headings, lists, and
paragraph boundaries.

## Why

PDFs do not carry semantic structure. Most extraction libraries either
return one undifferentiated text blob, or stop at the slow, general-purpose
parsing layer. `pdfmd` skips both: it owns the path from bytes to
Markdown, which keeps the conversion under ~7 ms for a typical academic
paper and leaves room to tune the heuristics for the documents you care
about.

## Install

```sh
cargo install --path .
```

Or build a release binary directly:

```sh
cargo build --release
# binary is at ./target/release/pdfmd
```

## Usage

```sh
pdfmd input.pdf                     # markdown to stdout
pdfmd input.pdf -o output.md        # write to a file
pdfmd input.pdf --page-breaks       # insert `---` between PDF pages
pdfmd input.pdf --extract-images figs -o out.md
                                    # save embedded JPEGs into ./figs
                                    # and link them inline in out.md
cat input.pdf | pdfmd -             # read from stdin
```

Image extraction currently passes through JPEG (`DCTDecode`) and JPEG 2000
(`JPXDecode`) streams verbatim; PDFs that store figures as `FlateDecode`
bitmaps or other filters will not yield image files.

## Performance

End-to-end CLI benchmark on a 1.05 MB, 17-page arXiv paper (Apple Silicon,
release build, `hyperfine --warmup 5 --runs 20`):

| metric     | value                |
|------------|----------------------|
| min        | 5.4 ms               |
| mean       | 6.2 ms ± 0.4 ms      |
| throughput | ~170 MB/s of PDF     |
| pages/sec  | ~2,750               |

Per-page font and content-stream work runs in parallel on
[`rayon`](https://crates.io/crates/rayon), and fonts shared across pages
are parsed once into a document-wide cache. The content-stream tokenizer
is hand-rolled and borrows operands directly from the source bytes, so
the hot path doesn't allocate per operator.

## How it works

```
src/
├── extract/
│   ├── encoding.rs    WinAnsi / MacRoman / Standard / Symbol byte → glyph
│   ├── glyphs.rs      glyph name → Unicode, with uniXXXX fallback
│   ├── cmap.rs        ToUnicode CMap parser (bfchar / bfrange)
│   ├── font.rs        per-font byte → text decoder
│   ├── parser.rs      streaming content-stream tokenizer
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
