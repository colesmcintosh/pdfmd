"""Tests for the ctypes bindings.

Run from the repository root after `cargo build --release`:

    PYTHONPATH=python python3 -m unittest discover -s python/tests
"""

from __future__ import annotations

import concurrent.futures
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import pdfmd  # noqa: E402
from pdfmd import _binding  # noqa: E402

FIXTURE = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "sample.pdf"


class ConvertTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        cls.data = FIXTURE.read_bytes()

    def test_converts_bytes_to_markdown(self) -> None:
        result = pdfmd.convert(self.data)
        self.assertIsInstance(result.markdown, str)
        self.assertTrue(result.markdown.startswith("# "))
        self.assertTrue(result.markdown.endswith("\n"))
        self.assertEqual(result.images, [])

    def test_accepts_bytearray_and_memoryview(self) -> None:
        expected = pdfmd.convert(self.data).markdown
        for payload in (bytearray(self.data), memoryview(self.data)):
            with self.subTest(kind=type(payload).__name__):
                self.assertEqual(pdfmd.convert(payload).markdown, expected)

    def test_convert_file_matches_convert(self) -> None:
        self.assertEqual(
            pdfmd.convert_file(FIXTURE).markdown, pdfmd.convert(self.data).markdown
        )

    def test_convert_file_accepts_a_string_path(self) -> None:
        self.assertTrue(pdfmd.convert_file(str(FIXTURE)).markdown)

    def test_page_breaks_insert_rules(self) -> None:
        self.assertNotIn("\n---\n", pdfmd.convert(self.data).markdown)
        self.assertIn("\n---\n", pdfmd.convert(self.data, page_breaks=True).markdown)

    def test_image_dir_extracts_images_and_rewrites_links(self) -> None:
        result = pdfmd.convert(self.data, image_dir="figs")
        self.assertTrue(result.images)
        for image in result.images:
            self.assertTrue(image.filename.startswith("img-"))
            self.assertTrue(image.data)
            self.assertIn(f"![](figs/{image.filename})", result.markdown)

    def test_write_images_creates_the_directory(self) -> None:
        result = pdfmd.convert(self.data, image_dir="figs")
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "nested" / "figs"
            written = result.write_images(target)
            self.assertEqual(len(written), len(result.images))
            for path, image in zip(written, result.images):
                self.assertEqual(path.read_bytes(), image.data)

    def test_write_images_with_no_images_only_creates_the_directory(self) -> None:
        result = pdfmd.convert(self.data)
        with tempfile.TemporaryDirectory() as tmp:
            target = Path(tmp) / "figs"
            self.assertEqual(result.write_images(target), [])
            self.assertTrue(target.is_dir())

    def test_rejects_non_pdf_input(self) -> None:
        with self.assertRaises(pdfmd.PdfError) as caught:
            pdfmd.convert(b"not a pdf at all")
        self.assertIn("does not look like a PDF", str(caught.exception))

    def test_rejects_empty_input(self) -> None:
        with self.assertRaises(pdfmd.PdfError):
            pdfmd.convert(b"")

    def test_rejects_a_missing_file(self) -> None:
        with self.assertRaises(FileNotFoundError):
            pdfmd.convert_file(FIXTURE.with_name("absent.pdf"))

    def test_converts_from_several_threads(self) -> None:
        # ctypes releases the GIL, so these overlap rather than serialize.
        with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
            results = list(pool.map(lambda _: pdfmd.convert(self.data), range(4)))
        self.assertEqual(len({r.markdown for r in results}), 1)


class MetadataTests(unittest.TestCase):
    def test_version_matches_the_crate(self) -> None:
        manifest = FIXTURE.parents[2] / "Cargo.toml"
        version = next(
            line.split('"')[1]
            for line in manifest.read_text().splitlines()
            if line.startswith("version = ")
        )
        self.assertEqual(pdfmd.__version__, version)

    def test_library_path_points_at_a_real_file(self) -> None:
        self.assertTrue(pdfmd.library_path().is_file())

    def test_repr_summarises_results_and_images(self) -> None:
        result = pdfmd.convert(FIXTURE.read_bytes(), image_dir="figs")
        self.assertIn("images", repr(result))
        self.assertIn("bytes", repr(result.images[0]))


class LibraryDiscoveryTests(unittest.TestCase):
    def test_env_override_wins(self) -> None:
        actual = pdfmd.library_path()
        os.environ["PDFMD_LIBRARY"] = str(actual)
        try:
            self.assertEqual(_binding.library_path(), actual)
        finally:
            del os.environ["PDFMD_LIBRARY"]

    def test_missing_library_reports_what_was_tried(self) -> None:
        os.environ["PDFMD_LIBRARY"] = str(FIXTURE.with_name("nope.so"))
        try:
            with self.assertRaises(ImportError) as caught:
                _binding.library_path()
            self.assertIn("nope.so", str(caught.exception))
        finally:
            del os.environ["PDFMD_LIBRARY"]


if __name__ == "__main__":
    unittest.main()
