"""Command line front end: ``python -m pdfmd``, and the ``pdfmd`` script.

Mirrors the Rust CLI's flags over the same shared library, so a wheel is
usable from a shell without a Rust toolchain. The one difference is how
URLs are fetched: the crate shells out to ``curl`` to stay dependency
free, while here ``urllib`` is already in the standard library.
"""

from __future__ import annotations

import argparse
import sys
import urllib.request
from pathlib import Path
from typing import BinaryIO, List, Optional

from . import PdfError, __version__, convert

USAGE = "pdfmd [OPTIONS] <INPUT>"


def _parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="pdfmd",
        usage=USAGE,
        description="Convert PDF documents to Markdown.",
    )
    parser.add_argument(
        "input",
        metavar="INPUT",
        help='path to the input PDF, an http(s):// URL, or "-" for stdin',
    )
    parser.add_argument(
        "-o",
        "--output",
        metavar="FILE",
        help="write Markdown to FILE instead of stdout",
    )
    parser.add_argument(
        "--page-breaks",
        action="store_true",
        help="insert `---` between PDF pages",
    )
    parser.add_argument(
        "--extract-images",
        metavar="DIR",
        help="save supported embedded images into DIR and reference them inline",
    )
    parser.add_argument(
        "-V",
        "--version",
        action="version",
        version=f"pdfmd {__version__}",
    )
    return parser


def _read(source: str, stdin: BinaryIO) -> bytes:
    if source == "-":
        return stdin.read()
    if source.startswith(("http://", "https://")):
        # The scheme is checked above, so this can't reach file:// or friends.
        with urllib.request.urlopen(source) as response:
            return response.read()
    return Path(source).read_bytes()


def main(
    argv: Optional[List[str]] = None,
    stdin: Optional[BinaryIO] = None,
    stdout: Optional[BinaryIO] = None,
) -> int:
    """Run one conversion. Returns the process exit code.

    The streams are arguments so the tests can drive this without
    replacing the real stdin and stdout.
    """
    args = _parser().parse_args(argv)
    stdin = stdin if stdin is not None else sys.stdin.buffer
    stdout = stdout if stdout is not None else sys.stdout.buffer

    try:
        data = _read(args.input, stdin)
    except OSError as e:
        print(f"error: failed to read {args.input}: {e}", file=sys.stderr)
        return 1

    try:
        result = convert(
            data, page_breaks=args.page_breaks, image_dir=args.extract_images
        )
    except PdfError as e:
        print(f"error: {e}", file=sys.stderr)
        return 1

    try:
        if args.extract_images:
            result.write_images(args.extract_images)
        if args.output:
            Path(args.output).write_text(result.markdown, encoding="utf-8")
        else:
            stdout.write(result.markdown.encode("utf-8"))
    except OSError as e:
        print(f"error: failed to write output: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":  # pragma: no cover - exercised as a subprocess
    raise SystemExit(main())
