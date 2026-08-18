//! End-to-end coverage for the public API and the CLI binary. Anything that
//! requires a real PDF on disk lives here so that the library unit tests can
//! stay focused on small in-process invariants.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Output};

fn reference_pdf() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/sample.pdf")
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_pdfmd"))
}

fn run_cli(args: impl IntoIterator<Item = OsString>) -> Output {
    Command::new(binary()).args(args).output().expect("spawn")
}

/// Assert the CLI exited successfully, and hand back its stdout.
fn assert_cli_ok(args: impl IntoIterator<Item = OsString>) -> String {
    let output = run_cli(args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Assert the CLI exited non-zero and said `needle` on stderr.
fn assert_cli_error(args: impl IntoIterator<Item = OsString>, needle: &str) {
    let output = run_cli(args);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(needle),
        "stderr lacked {needle:?}: {stderr}"
    );
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

#[test]
fn converts_page_without_contents_to_empty_markdown() {
    let bytes = build_xref_pdf(
        b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<<>>/MediaBox[0 0 1 1]>> endobj
",
    );
    let out =
        pdfmd::convert_pdf_to_markdown(&bytes, &pdfmd::ConvertOptions::default()).expect("convert");
    assert!(out.markdown.trim().is_empty());
    assert!(out.images.is_empty());
}

#[test]
fn converts_stored_flate_content_stream() {
    let content = b"BT /F1 12 Tf (Hi) Tj ET";
    let compressed = zlib_stored(content);
    let bytes = flate_text_pdf(&compressed);
    let out =
        pdfmd::convert_pdf_to_markdown(&bytes, &pdfmd::ConvertOptions::default()).expect("convert");
    assert!(out.markdown.contains("Hi"));
}

#[test]
fn converts_multi_block_stored_flate_content_stream() {
    let compressed = zlib_two_stored_blocks(b"BT /F1 12 Tf ", b"(Hi) Tj ET");
    let bytes = flate_text_pdf(&compressed);
    let out =
        pdfmd::convert_pdf_to_markdown(&bytes, &pdfmd::ConvertOptions::default()).expect("convert");
    assert!(out.markdown.contains("Hi"));
}

#[test]
fn tolerates_truncated_stored_flate_content_streams() {
    for raw in [
        &[0x01][..],
        &[0x01, 0x00][..],
        &[0x01, 0x05, 0x00, 0xFA, 0xFF, b'A', b'B'][..],
    ] {
        let mut compressed = vec![0x78, 0x01];
        compressed.extend_from_slice(raw);
        compressed.extend_from_slice(&[0, 0, 0, 1]);
        let bytes = flate_text_pdf(&compressed);
        let out = pdfmd::convert_pdf_to_markdown(&bytes, &pdfmd::ConvertOptions::default())
            .expect("convert");
        assert!(out.markdown.trim().is_empty());
    }
}

// ---- CLI binary ------------------------------------------------------------

#[test]
fn cli_writes_markdown_to_file() {
    let tmp = tempdir();
    let out = tmp.join("out.md");
    assert_cli_ok([reference_pdf().into(), "-o".into(), out.clone().into()]);
    assert!(!std::fs::read_to_string(&out)
        .expect("read output")
        .is_empty());
}

#[test]
fn cli_converts_multi_block_stored_flate_pdf() {
    let tmp = tempdir();
    let input = tmp.join("multi-block.pdf");
    let out = tmp.join("out.md");
    let pdf = flate_text_pdf(&zlib_two_stored_blocks(b"BT /F1 12 Tf ", b"(Hi) Tj ET"));
    std::fs::write(&input, pdf).unwrap();

    assert_cli_ok([input.into(), "-o".into(), out.clone().into()]);
    assert!(std::fs::read_to_string(&out).unwrap().contains("Hi"));
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

#[cfg(unix)]
#[test]
fn cli_reports_stdout_write_error_when_pipe_is_closed() {
    // Windows pipe-close timing can let the child write before the parent
    // observes the closed reader; Unix gives us the deterministic error.
    let mut child = Command::new(binary())
        .arg(reference_pdf())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    drop(child.stdout.take());
    let output = child.wait_with_output().expect("wait");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("failed to write to stdout"));
}

#[test]
fn cli_url_input_reports_fetch_errors() {
    let url = "http://127.0.0.1:1/pdfmd-nope.pdf";
    assert_cli_error([url.into()], "fetch failed");
    assert_cli_error([url.into()], "127.0.0.1");
}

#[cfg(unix)]
#[test]
fn cli_url_input_reports_missing_curl() {
    // Windows searches system directories for executables even with PATH
    // cleared, so this subprocess-only assertion is Unix-specific.
    let output = Command::new(binary())
        .arg("http://example.invalid/pdfmd-nope.pdf")
        .env("PATH", "")
        .output()
        .expect("spawn");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("could not run `curl`"));
}

#[test]
fn cli_extract_images_reports_write_error() {
    let tmp = tempdir();
    let blocker = tmp.join("not-a-dir");
    std::fs::write(&blocker, b"file").unwrap();
    assert_cli_error(
        [
            reference_pdf().into(),
            "--extract-images".into(),
            blocker.into(),
            "-o".into(),
            tmp.join("out.md").into(),
        ],
        "failed to write images",
    );
}

#[cfg(unix)]
#[test]
fn cli_extract_images_rejects_non_utf8_path() {
    use std::os::unix::ffi::OsStringExt;

    assert_cli_error(
        [
            reference_pdf().into(),
            "--extract-images".into(),
            OsString::from_vec(vec![0xFF]),
        ],
        "must be valid UTF-8",
    );
}

#[cfg(unix)]
#[test]
fn cli_handles_non_utf8_positional_args_without_panicking() {
    use std::os::unix::ffi::OsStringExt;

    let bad_path = OsString::from_vec(vec![0xFF]);
    assert_cli_error([bad_path.clone()], "failed to read");
    assert_cli_error([reference_pdf().into(), bad_path], "unexpected positional");
}

#[test]
fn cli_help_exits_clean() {
    let help = assert_cli_ok(["--help".into()]);
    assert!(help.contains("USAGE"));
    assert!(help.contains("JPEG 2000"));
    assert!(help.contains("PNG"));
    assert_cli_ok(["-h".into()]);
}

#[test]
fn cli_version_prints_pkg_version() {
    assert!(assert_cli_ok(["--version".into()]).starts_with("pdfmd "));
    assert_cli_ok(["-V".into()]);
}

#[test]
fn cli_extract_images_creates_directory() {
    let tmp = tempdir();
    let figs = tmp.join("figs");
    assert_cli_ok([
        reference_pdf().into(),
        "--page-breaks".into(),
        "--extract-images".into(),
        figs.clone().into(),
        "-o".into(),
        tmp.join("out.md").into(),
    ]);
    assert!(figs.exists());
    // Equals-form should work too.
    assert_cli_ok([
        reference_pdf().into(),
        format!("--output={}", tmp.join("out2.md").display()).into(),
        format!("--extract-images={}", figs.display()).into(),
    ]);
}

#[test]
fn cli_rejects_malformed_invocations() {
    assert_cli_error([], "missing");
    assert_cli_error(["--no-such-flag".into()], "unknown flag");
    assert_cli_error(["-o".into()], "missing value for --output");
    assert_cli_error(
        ["--extract-images".into()],
        "missing value for --extract-images",
    );
    assert_cli_error(
        [reference_pdf().into(), "also-this.pdf".into()],
        "unexpected positional",
    );
}

#[test]
fn cli_propagates_io_error_for_missing_file() {
    assert_cli_error(["/definitely/does/not/exist.pdf".into()], "failed to read");
}

#[test]
fn cli_reports_output_write_error_for_unwritable_path() {
    let tmp = tempdir();
    let out_dir = tmp.join("out.md");
    std::fs::create_dir(&out_dir).unwrap();
    assert_cli_error(
        [reference_pdf().into(), "-o".into(), out_dir.into()],
        "failed to write",
    );
}

#[test]
fn cli_propagates_pdf_error_for_garbage_input() {
    let path = tempdir().join("not.pdf");
    std::fs::write(&path, b"not even close").unwrap();
    assert_cli_error([path.into()], "does not look like a PDF");
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

fn build_xref_pdf(body: &[u8]) -> Vec<u8> {
    let mut out = body.to_vec();
    let xref_offset = out.len();
    let mut found: Vec<(u32, usize)> = Vec::new();
    for n in 1u32..200 {
        let needle = format!("{n} 0 obj");
        if let Some(off) = (0..=out.len().saturating_sub(needle.len()))
            .find(|&i| out[i..i + needle.len()] == *needle.as_bytes())
        {
            found.push((n, off));
        }
    }

    let max = found.iter().map(|(n, _)| *n).max().unwrap_or(0);
    let mut xref = String::from("xref\n");
    xref.push_str(&format!("0 {}\n", max + 1));
    xref.push_str("0000000000 65535 f \n");
    for n in 1..=max {
        match found.iter().find(|(m, _)| *m == n) {
            Some((_, off)) => xref.push_str(&format!("{off:010} 00000 n \n")),
            None => xref.push_str("0000000000 00000 f \n"),
        }
    }
    xref.push_str(&format!(
        "trailer <</Size {}/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n",
        max + 1
    ));
    out.extend_from_slice(xref.as_bytes());
    out
}

fn flate_text_pdf(compressed: &[u8]) -> Vec<u8> {
    let mut body = b"\
%PDF-1.4
1 0 obj <</Type/Catalog/Pages 2 0 R>> endobj
2 0 obj <</Type/Pages/Kids[3 0 R]/Count 1>> endobj
3 0 obj <</Type/Page/Parent 2 0 R/Resources<</Font<</F1 5 0 R>>>>/MediaBox[0 0 1 1]/Contents 4 0 R>> endobj
4 0 obj <</Filter/FlateDecode/Length "
        .to_vec();
    body.extend_from_slice(compressed.len().to_string().as_bytes());
    body.extend_from_slice(b">>\nstream\n");
    body.extend_from_slice(compressed);
    body.extend_from_slice(
        b"\nendstream\nendobj\n5 0 obj <</Type/Font/Subtype/Type1/BaseFont/Helvetica>> endobj\n",
    );
    build_xref_pdf(&body)
}

fn zlib_stored(payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    push_stored_block(&mut out, true, payload);
    out.extend_from_slice(&[0, 0, 0, 1]);
    out
}

fn zlib_two_stored_blocks(first: &[u8], second: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    push_stored_block(&mut out, false, first);
    push_stored_block(&mut out, true, second);
    out.extend_from_slice(&[0, 0, 0, 1]);
    out
}

fn push_stored_block(out: &mut Vec<u8>, final_block: bool, payload: &[u8]) {
    assert!(payload.len() <= u16::MAX as usize);
    let len = payload.len() as u16;
    out.push(u8::from(final_block));
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
    out.extend_from_slice(payload);
}
