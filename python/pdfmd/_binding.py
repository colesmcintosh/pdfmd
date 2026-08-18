"""Locate and describe the ``pdfmd`` shared library.

The bindings are ``ctypes`` over the C ABI in ``src/ffi.rs`` rather than a
compiled extension module, so the Python side stays pure Python and the Rust
side keeps its empty dependency list.
"""

from __future__ import annotations

import ctypes
import os
import sys
from ctypes.util import find_library
from pathlib import Path

__all__ = ["PdfmdImage", "PdfmdResult", "library", "library_path"]


class PdfmdImage(ctypes.Structure):
    """Mirror of ``ffi::PdfmdImage``."""

    _fields_ = [
        ("filename", ctypes.POINTER(ctypes.c_ubyte)),
        ("filename_len", ctypes.c_size_t),
        ("bytes", ctypes.POINTER(ctypes.c_ubyte)),
        ("bytes_len", ctypes.c_size_t),
    ]


class PdfmdResult(ctypes.Structure):
    """Mirror of ``ffi::PdfmdResult``."""

    _fields_ = [
        ("markdown", ctypes.POINTER(ctypes.c_ubyte)),
        ("markdown_len", ctypes.c_size_t),
        ("images", ctypes.POINTER(PdfmdImage)),
        ("image_count", ctypes.c_size_t),
        ("error", ctypes.POINTER(ctypes.c_ubyte)),
        ("error_len", ctypes.c_size_t),
        ("owner", ctypes.c_void_p),
    ]


def _filenames() -> tuple[str, ...]:
    if sys.platform == "darwin":
        return ("libpdfmd.dylib",)
    if os.name == "nt":
        return ("pdfmd.dll", "libpdfmd.dll")
    return ("libpdfmd.so",)


def _candidates() -> list[Path]:
    """Every path we are willing to load, most specific first."""
    override = os.environ.get("PDFMD_LIBRARY")
    if override:
        return [Path(override)]

    here = Path(__file__).resolve().parent
    # An installed wheel carries the library next to this module. A source
    # checkout does not, so fall back to whatever cargo last built.
    roots = [here, *(here.parents[1] / "target" / p for p in ("release", "debug"))]
    return [root / name for root in roots for name in _filenames()]


def library_path() -> Path:
    """Path of the shared library that :func:`library` will load."""
    for candidate in _candidates():
        if candidate.is_file():
            return candidate

    found = find_library("pdfmd")
    if found:
        return Path(found)

    tried = "\n  ".join(str(c) for c in _candidates())
    raise ImportError(
        "could not find the pdfmd shared library. Build it with "
        "`cargo build --release` from the repository root, or point "
        "PDFMD_LIBRARY at the file. Tried:\n  " + tried
    )


def _bind(lib: ctypes.CDLL) -> ctypes.CDLL:
    lib.pdfmd_version.argtypes = []
    lib.pdfmd_version.restype = ctypes.c_char_p
    lib.pdfmd_convert.argtypes = [
        ctypes.POINTER(ctypes.c_ubyte),
        ctypes.c_size_t,
        ctypes.c_bool,
        ctypes.c_char_p,
    ]
    lib.pdfmd_convert.restype = ctypes.POINTER(PdfmdResult)
    lib.pdfmd_result_free.argtypes = [ctypes.POINTER(PdfmdResult)]
    lib.pdfmd_result_free.restype = None
    return lib


_LIBRARY: ctypes.CDLL | None = None


def library() -> ctypes.CDLL:
    """Load the shared library once and cache it.

    ``CDLL`` releases the GIL for the duration of each call, so conversions
    running on separate threads overlap with each other and with Python.
    """
    global _LIBRARY
    if _LIBRARY is None:
        _LIBRARY = _bind(ctypes.CDLL(str(library_path())))
    return _LIBRARY
