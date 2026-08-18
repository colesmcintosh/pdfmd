"""Build the crate's cdylib and ship it inside the wheel.

There is no compiled extension module here — the wheel carries the same
shared library `cargo build --release` produces, and `pdfmd/_binding.py`
loads it with `ctypes`. That keeps the Python side dependency-free at both
build and run time, matching the crate.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

from setuptools import Distribution, setup
from setuptools.command.build_py import build_py
from setuptools.command.bdist_wheel import bdist_wheel

HERE = Path(__file__).resolve().parent
CRATE = HERE.parent


def crate_version(default: str = "0.0.0") -> str:
    """Single source of truth for the version: the crate manifest."""
    manifest = CRATE / "Cargo.toml"
    if not manifest.is_file():
        return os.environ.get("PDFMD_VERSION", default)
    match = re.search(r'^version\s*=\s*"([^"]+)"', manifest.read_text(), re.M)
    return match.group(1) if match else default


def library_name() -> str:
    if sys.platform == "darwin":
        return "libpdfmd.dylib"
    if os.name == "nt":
        return "pdfmd.dll"
    return "libpdfmd.so"


def build_library() -> Path:
    """Return the shared library, building it with cargo unless one is given."""
    prebuilt = os.environ.get("PDFMD_LIBRARY")
    if prebuilt:
        return Path(prebuilt)

    if not (CRATE / "Cargo.toml").is_file():
        raise SystemExit(
            "the crate sources are not next to this package; set PDFMD_LIBRARY "
            "to a prebuilt shared library instead"
        )
    subprocess.run(
        ["cargo", "build", "--release", "--manifest-path", str(CRATE / "Cargo.toml")],
        check=True,
    )
    return CRATE / "target" / "release" / library_name()


class BuildPyWithLibrary(build_py):
    def run(self) -> None:
        super().run()
        library = build_library()
        if not library.is_file():
            raise SystemExit(f"expected a shared library at {library}")
        destination = Path(self.build_lib) / "pdfmd" / library_name()
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(library, destination)


class WheelWithLibrary(bdist_wheel):
    """Tag the wheel `py3-none-<platform>`.

    The payload is a native library, so the wheel is platform-specific — but
    nothing here is compiled against a Python ABI, so one wheel per platform
    serves every Python 3.
    """

    def finalize_options(self) -> None:
        super().finalize_options()
        self.root_is_pure = False

    def get_tag(self) -> tuple[str, str, str]:
        _, _, platform = super().get_tag()
        return "py3", "none", platform


class BinaryDistribution(Distribution):
    """Install into platlib, and keep the package at the wheel root."""

    def has_ext_modules(self) -> bool:
        return True


setup(
    version=crate_version(),
    cmdclass={"build_py": BuildPyWithLibrary, "bdist_wheel": WheelWithLibrary},
    distclass=BinaryDistribution,
)
