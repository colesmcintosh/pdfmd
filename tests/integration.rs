//! End-to-end coverage for the public API and the CLI binary. Anything that
//! requires a real PDF on disk lives here so that the library unit tests can
//! stay focused on small in-process invariants.

use std::path::PathBuf;
use std::process::Command;

fn reference_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf")
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pdfmd"))
}

// ---- Library surface -------------------------------------------------------

#[test]
fn converts_the_bundled_reference_pdf() {
    let bytes = std::fs::read(reference_pdf()).expect("read fixture");
    let opts = pdfmd::ConvertOptions::default();
    let out = pdfmd::convert_pdf_to_markdown(&bytes, &opts).expect("convert");
    assert!(!out.markdown.is_empty());
    assert!(out.markdown.contains("INTRODUCTION") || out.markdown.contains("Introduction"));
    assert!(out.markdown.ends_with('\n'));
}

#[test]
fn page_breaks_insert_horizontal_rules() {
    let bytes = std::fs::read(reference_pdf()).expect("read fixture");
    let with = pdfmd::convert_pdf_to_markdown(
        &bytes,
        &pdfmd::ConvertOptions {
            include_page_breaks: true,
            image_dir: None,
        },
    )
    .expect("convert");
    let without =
        pdfmd::convert_pdf_to_markdown(&bytes, &pdfmd::ConvertOptions::default()).expect("convert");
    assert!(with.markdown.contains("\n\n---\n\n"));
    assert!(!without.markdown.contains("\n\n---\n\n"));
}

#[test]
fn extract_images_returns_pass_through_payloads() {
    let bytes = std::fs::read(reference_pdf()).expect("read fixture");
    let result = pdfmd::convert_pdf_to_markdown(
        &bytes,
        &pdfmd::ConvertOptions {
            include_page_breaks: false,
            image_dir: Some("figs"),
        },
    )
    .expect("convert");
    // The reference PDF stores images as FlateDecode, not DCT/JPX, so no
    // pass-through extraction happens. We still want this code path
    // exercised end-to-end with `image_dir = Some(...)`.
    for img in &result.images {
        assert!(!img.bytes.is_empty());
        assert!(img.filename.starts_with("img-"));
    }
}

#[test]
fn rejects_non_pdf_input() {
    let err = match pdfmd::convert_pdf_to_markdown(b"not a pdf", &pdfmd::ConvertOptions::default())
    {
        Ok(_) => panic!("expected error"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("does not look like a PDF"));
}

// ---- CLI binary ------------------------------------------------------------

#[test]
fn cli_writes_markdown_to_file() {
    let tmp = tempdir();
    let out = tmp.join("out.md");
    let status = Command::new(binary())
        .arg(reference_pdf())
        .arg("-o")
        .arg(&out)
        .status()
        .expect("spawn");
    assert!(status.success());
    let md = std::fs::read_to_string(&out).expect("read output");
    assert!(!md.is_empty());
}

#[test]
fn cli_streams_from_stdin_to_stdout() {
    let bytes = std::fs::read(reference_pdf()).expect("read fixture");
    let mut child = Command::new(binary())
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    use std::io::Write;
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(&bytes)
        .expect("write stdin");
    let output = child.wait_with_output().expect("wait");
    assert!(output.status.success());
    assert!(!output.stdout.is_empty());
}

#[test]
fn cli_help_exits_clean() {
    let output = Command::new(binary())
        .arg("--help")
        .output()
        .expect("spawn");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("USAGE"));

    let output = Command::new(binary()).arg("-h").output().expect("spawn");
    assert!(output.status.success());
}

#[test]
fn cli_version_prints_pkg_version() {
    let output = Command::new(binary())
        .arg("--version")
        .output()
        .expect("spawn");
    assert!(output.status.success());
    let s = String::from_utf8_lossy(&output.stdout);
    assert!(s.starts_with("pdfmd "));

    let output = Command::new(binary()).arg("-V").output().expect("spawn");
    assert!(output.status.success());
}

#[test]
fn cli_extract_images_creates_directory() {
    let tmp = tempdir();
    let out = tmp.join("out.md");
    let figs = tmp.join("figs");
    let status = Command::new(binary())
        .arg(reference_pdf())
        .arg("--page-breaks")
        .arg("--extract-images")
        .arg(&figs)
        .arg("-o")
        .arg(&out)
        .status()
        .expect("spawn");
    assert!(status.success());
    assert!(figs.exists());
    // Equals-form should work too.
    let out2 = tmp.join("out2.md");
    let status = Command::new(binary())
        .arg(reference_pdf())
        .arg(format!("--output={}", out2.display()))
        .arg(format!("--extract-images={}", figs.display()))
        .status()
        .expect("spawn");
    assert!(status.success());
}

#[test]
fn cli_missing_input_errors() {
    let output = Command::new(binary()).output().expect("spawn");
    assert!(!output.status.success());
    let s = String::from_utf8_lossy(&output.stderr);
    assert!(s.contains("missing"));
}

#[test]
fn cli_unknown_flag_errors() {
    let output = Command::new(binary())
        .arg("--no-such-flag")
        .output()
        .expect("spawn");
    assert!(!output.status.success());
}

#[test]
fn cli_missing_value_for_output_errors() {
    let output = Command::new(binary()).arg("-o").output().expect("spawn");
    assert!(!output.status.success());
    let output = Command::new(binary())
        .arg("--extract-images")
        .output()
        .expect("spawn");
    assert!(!output.status.success());
}

#[test]
fn cli_extra_positional_errors() {
    let output = Command::new(binary())
        .arg(reference_pdf())
        .arg("also-this.pdf")
        .output()
        .expect("spawn");
    assert!(!output.status.success());
}

#[test]
fn cli_propagates_io_error_for_missing_file() {
    let output = Command::new(binary())
        .arg("/definitely/does/not/exist.pdf")
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    let s = String::from_utf8_lossy(&output.stderr);
    assert!(s.contains("failed to read"));
}

#[test]
fn cli_propagates_pdf_error_for_garbage_input() {
    let tmp = tempdir();
    let path = tmp.join("not.pdf");
    std::fs::write(&path, b"not even close").unwrap();
    let output = Command::new(binary()).arg(&path).output().expect("spawn");
    assert!(!output.status.success());
}

// ---- Tiny private tempdir helper (no extra dependency) --------------------

fn tempdir() -> PathBuf {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let p = std::env::temp_dir().join(format!("pdfmd-tests-{nanos}-{}", std::process::id()));
    std::fs::create_dir_all(&p).expect("mk tempdir");
    p
}
