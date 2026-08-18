"""Convert PDF documents into Markdown.

Python bindings for the ``pdfmd`` Rust crate, over its C ABI:

    >>> import pdfmd
    >>> result = pdfmd.convert_file("paper.pdf")
    >>> print(result.markdown)

:func:`convert_many` converts a whole batch on a thread pool, and
``python -m pdfmd`` (or the ``pdfmd`` script an installed wheel puts on
PATH) runs the same conversion from a shell.

Images are extracted only when ``image_dir`` is given. That value is the
directory the Markdown links point at; writing the files is the caller's
job, either directly or with :meth:`ConvertResult.write_images`.
"""

from __future__ import annotations

import concurrent.futures
import ctypes
import os
from pathlib import Path
from typing import Iterable, Iterator, List, Optional, Union

from . import _binding

__all__ = [
    "ConvertResult",
    "Image",
    "PdfError",
    "convert",
    "convert_file",
    "convert_many",
    "library_path",
    "__version__",
]

StrPath = Union[str, "os.PathLike[str]"]
Data = Union[bytes, bytearray, memoryview]
Source = Union[StrPath, Data]
"""One input to :func:`convert_many`: a path, or the bytes of a PDF."""


class PdfError(Exception):
    """Raised when a PDF cannot be converted.

    Mirrors the crate's single error type: malformed files, unsupported
    filters (``LZWDecode``), and encrypted documents all arrive here.
    """


class Image:
    """One image extracted from the PDF, ready to be written to disk."""

    __slots__ = ("filename", "data")

    def __init__(self, filename: str, data: bytes) -> None:
        self.filename = filename
        """``img-NNN.ext``, matching the link already in the Markdown."""
        self.data = data
        """Encoded file bytes — JPEG, JPEG 2000, or PNG."""

    def __repr__(self) -> str:
        return f"Image(filename={self.filename!r}, {len(self.data)} bytes)"


class ConvertResult:
    """Markdown plus any images extracted alongside it."""

    __slots__ = ("markdown", "images")

    def __init__(self, markdown: str, images: List[Image]) -> None:
        self.markdown = markdown
        self.images = images

    def __repr__(self) -> str:
        return (
            f"ConvertResult({len(self.markdown)} chars, {len(self.images)} images)"
        )

    def write_images(self, directory: StrPath) -> List[Path]:
        """Write every extracted image into ``directory``, creating it.

        Pass the same directory given as ``image_dir`` (or a path ending in
        it) so the links in the Markdown resolve.
        """
        target = Path(directory)
        target.mkdir(parents=True, exist_ok=True)
        written = []
        for image in self.images:
            path = target / image.filename
            path.write_bytes(image.data)
            written.append(path)
        return written


def library_path() -> Path:
    """Path of the shared library backing these bindings."""
    return _binding.library_path()


def _buffer(pointer, length: int) -> bytes:
    return ctypes.string_at(pointer, length) if length else b""


def convert(
    data: Union[bytes, bytearray, memoryview],
    *,
    page_breaks: bool = False,
    image_dir: Optional[str] = None,
) -> ConvertResult:
    """Convert the bytes of a PDF into Markdown.

    ``page_breaks`` inserts a ``---`` rule between pages. ``image_dir`` is
    the directory name embedded in ``![](dir/img-NNN.ext)`` links; when it is
    ``None`` images are ignored entirely.
    """
    # `bytes(...)` is a no-op for bytes input, and the cast below hands the
    # pointer over without a second copy of what may be a large file.
    raw = data if isinstance(data, bytes) else bytes(data)
    lib = _binding.library()

    encoded_dir = image_dir.encode("utf-8") if image_dir is not None else None
    buffer = ctypes.cast(ctypes.c_char_p(raw), ctypes.POINTER(ctypes.c_ubyte))
    result = lib.pdfmd_convert(buffer, len(raw), page_breaks, encoded_dir)
    if not result:
        raise PdfError("pdfmd_convert returned no result")

    try:
        payload = result.contents
        if payload.error:
            raise PdfError(_buffer(payload.error, payload.error_len).decode("utf-8"))

        markdown = _buffer(payload.markdown, payload.markdown_len).decode("utf-8")
        images = [
            Image(
                _buffer(image.filename, image.filename_len).decode("utf-8"),
                _buffer(image.bytes, image.bytes_len),
            )
            for image in _image_views(payload)
        ]
    finally:
        lib.pdfmd_result_free(result)

    return ConvertResult(markdown, images)


def _image_views(payload: "_binding.PdfmdResult") -> Iterator["_binding.PdfmdImage"]:
    for index in range(payload.image_count):
        yield payload.images[index]


def convert_file(
    path: StrPath,
    *,
    page_breaks: bool = False,
    image_dir: Optional[str] = None,
) -> ConvertResult:
    """Read a PDF from disk and convert it. See :func:`convert`."""
    return convert(
        Path(path).read_bytes(), page_breaks=page_breaks, image_dir=image_dir
    )


def convert_many(
    sources: Iterable[Source],
    *,
    page_breaks: bool = False,
    image_dir: Optional[str] = None,
    workers: Optional[int] = None,
) -> List[ConvertResult]:
    """Convert several PDFs at once, returning results in input order.

    Each source is a path or the bytes of a PDF, converted as
    :func:`convert_file` or :func:`convert` would. The work runs on a
    thread pool: the shared library releases the GIL for the length of
    every call, so conversions overlap on all cores instead of queueing
    behind each other.

    ``workers`` defaults to one per CPU, capped at the number of sources.
    A single conversion already fans its pages out across cores, so a
    larger pool mostly helps when the documents are short.

    The first source that fails raises, exactly as converting it alone
    would; the rest of the batch is abandoned.
    """
    if workers is not None and workers < 1:
        raise ValueError(f"workers must be at least 1, got {workers}")

    items = list(sources)
    if not items:
        return []

    def one(source: Source) -> ConvertResult:
        if isinstance(source, (bytes, bytearray, memoryview)):
            return convert(source, page_breaks=page_breaks, image_dir=image_dir)
        return convert_file(source, page_breaks=page_breaks, image_dir=image_dir)

    count = min(len(items), workers or (os.cpu_count() or 1))
    if count == 1:
        return [one(item) for item in items]
    with concurrent.futures.ThreadPoolExecutor(max_workers=count) as pool:
        # `map` yields in input order and re-raises the first failure.
        return list(pool.map(one, items))


def _version() -> str:
    return _binding.library().pdfmd_version().decode("utf-8")


try:
    __version__ = _version()
except (ImportError, OSError):  # pragma: no cover - library missing at import
    __version__ = "unknown"
