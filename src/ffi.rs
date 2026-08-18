//! A C ABI over [`crate::convert_pdf_to_markdown`], for callers outside Rust.
//!
//! The Python package in `python/` binds this with `ctypes`; anything that
//! can call C can use it too. Keeping the boundary at plain pointers and
//! lengths is what lets the crate ship bindings without taking on a
//! binding-generator dependency.
//!
//! Every buffer handed out is `(pointer, length)` — no NUL terminator is
//! implied, because extracted text may legitimately contain any byte.
//! [`pdfmd_convert`] always returns a non-null [`PdfmdResult`] that the
//! caller must hand back to [`pdfmd_result_free`] exactly once.
//!
//! ```c
//! PdfmdResult *r = pdfmd_convert(bytes, len, 0, NULL);
//! if (r->error) { fwrite(r->error, 1, r->error_len, stderr); }
//! else          { fwrite(r->markdown, 1, r->markdown_len, stdout); }
//! pdfmd_result_free(r);
//! ```

use std::ffi::c_void;
use std::os::raw::c_char;

use crate::{convert_pdf_to_markdown, ConvertOptions, ExtractedImage};

/// One extracted image: a borrowed view into the owning [`PdfmdResult`].
#[repr(C)]
pub struct PdfmdImage {
    /// `img-NNN.ext`, UTF-8, not NUL-terminated.
    pub filename: *const u8,
    pub filename_len: usize,
    /// Encoded file bytes, ready to write to `dir/filename`.
    pub bytes: *const u8,
    pub bytes_len: usize,
}

/// The outcome of one conversion. Exactly one of `markdown` and `error` is
/// non-null; `images` is non-null only when `image_count` is greater than 0.
#[repr(C)]
pub struct PdfmdResult {
    pub markdown: *const u8,
    pub markdown_len: usize,
    pub images: *const PdfmdImage,
    pub image_count: usize,
    /// UTF-8 message, not NUL-terminated. Null when the conversion succeeded.
    pub error: *const u8,
    pub error_len: usize,
    /// Opaque handle owning every buffer above. Do not dereference; pass the
    /// whole result back to [`pdfmd_result_free`].
    owner: *mut c_void,
}

/// Backing storage for the pointers in a [`PdfmdResult`]. Boxed before any
/// pointer is taken, so the heap buffers stay put for the caller's lifetime.
struct Owned {
    /// Markdown on success, the error message on failure.
    text: String,
    images: Vec<ExtractedImage>,
    views: Vec<PdfmdImage>,
}

/// The crate version as a NUL-terminated C string.
///
/// The returned pointer is static and must not be freed.
#[no_mangle]
pub extern "C" fn pdfmd_version() -> *const c_char {
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Convert `len` bytes of PDF at `bytes` into Markdown.
///
/// `page_breaks` mirrors [`ConvertOptions::include_page_breaks`]. `image_dir`
/// is either null (images ignored) or a NUL-terminated UTF-8 path used as the
/// prefix of the `![](dir/file)` links; the images themselves come back in
/// the result for the caller to write.
///
/// Never returns null. The result must be released with
/// [`pdfmd_result_free`].
///
/// # Safety
///
/// `bytes` must point to `len` readable bytes (or be null, which is reported
/// as an error), and `image_dir`, when non-null, must point to a
/// NUL-terminated string. Both only need to stay valid for this call.
#[no_mangle]
pub unsafe extern "C" fn pdfmd_convert(
    bytes: *const u8,
    len: usize,
    page_breaks: bool,
    image_dir: *const c_char,
) -> *mut PdfmdResult {
    if bytes.is_null() && len != 0 {
        return failure("null input pointer".to_string());
    }

    let dir = if image_dir.is_null() {
        None
    } else {
        match std::ffi::CStr::from_ptr(image_dir).to_str() {
            Ok(s) => Some(s),
            Err(_) => return failure("image_dir is not valid UTF-8".to_string()),
        }
    };

    let input = if bytes.is_null() {
        &[][..]
    } else {
        std::slice::from_raw_parts(bytes, len)
    };
    let opts = ConvertOptions {
        include_page_breaks: page_breaks,
        image_dir: dir,
    };

    match convert_pdf_to_markdown(input, &opts) {
        Ok(result) => success(result.markdown, result.images),
        Err(e) => failure(e.to_string()),
    }
}

/// Release a result returned by [`pdfmd_convert`].
///
/// # Safety
///
/// `result` must be a pointer from [`pdfmd_convert`] that has not already
/// been freed. Null is accepted and ignored.
#[no_mangle]
pub unsafe extern "C" fn pdfmd_result_free(result: *mut PdfmdResult) {
    if result.is_null() {
        return;
    }
    let result = Box::from_raw(result);
    drop(Box::from_raw(result.owner as *mut Owned));
}

fn success(markdown: String, images: Vec<ExtractedImage>) -> *mut PdfmdResult {
    // Box first: the pointers below have to name the final heap addresses.
    let mut owned = Box::new(Owned {
        text: markdown,
        images,
        views: Vec::new(),
    });
    owned.views = owned
        .images
        .iter()
        .map(|img| PdfmdImage {
            filename: img.filename.as_ptr(),
            filename_len: img.filename.len(),
            bytes: img.bytes.as_ptr(),
            bytes_len: img.bytes.len(),
        })
        .collect();

    // An empty `Vec` still yields a dangling non-null pointer; hand out null
    // instead so callers can branch on the pointer alone.
    let images = if owned.views.is_empty() {
        std::ptr::null()
    } else {
        owned.views.as_ptr()
    };
    let result = PdfmdResult {
        markdown: owned.text.as_ptr(),
        markdown_len: owned.text.len(),
        images,
        image_count: owned.views.len(),
        error: std::ptr::null(),
        error_len: 0,
        owner: Box::into_raw(owned) as *mut c_void,
    };
    Box::into_raw(Box::new(result))
}

fn failure(message: String) -> *mut PdfmdResult {
    let owned = Box::new(Owned {
        text: message,
        images: Vec::new(),
        views: Vec::new(),
    });
    let result = PdfmdResult {
        markdown: std::ptr::null(),
        markdown_len: 0,
        images: std::ptr::null(),
        image_count: 0,
        error: owned.text.as_ptr(),
        error_len: owned.text.len(),
        owner: Box::into_raw(owned) as *mut c_void,
    };
    Box::into_raw(Box::new(result))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Vec<u8> {
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/sample.pdf"
        ))
        .expect("read fixture")
    }

    /// Read a result's markdown / error pair back into Rust for assertions.
    unsafe fn parts(result: *const PdfmdResult) -> (Option<String>, Option<String>) {
        let r = &*result;
        let read = |ptr: *const u8, len: usize| {
            (!ptr.is_null())
                .then(|| String::from_utf8(std::slice::from_raw_parts(ptr, len).to_vec()).unwrap())
        };
        (read(r.markdown, r.markdown_len), read(r.error, r.error_len))
    }

    #[test]
    fn version_is_the_crate_version() {
        let s = unsafe { std::ffi::CStr::from_ptr(pdfmd_version()) };
        assert_eq!(s.to_str().unwrap(), env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn convert_returns_markdown_and_no_error() {
        let bytes = fixture();
        unsafe {
            let result = pdfmd_convert(bytes.as_ptr(), bytes.len(), false, std::ptr::null());
            let (markdown, error) = parts(result);
            assert!(error.is_none());
            assert!(markdown.unwrap().ends_with('\n'));
            assert_eq!((*result).image_count, 0);
            pdfmd_result_free(result);
        }
    }

    #[test]
    fn convert_reports_page_breaks_and_image_dir() {
        let bytes = fixture();
        let dir = std::ffi::CString::new("figs").unwrap();
        unsafe {
            let result = pdfmd_convert(bytes.as_ptr(), bytes.len(), true, dir.as_ptr());
            let (markdown, error) = parts(result);
            assert!(error.is_none());
            assert!(!markdown.unwrap().is_empty());
            // Every image view has to point at the owning result's buffers.
            let views = std::slice::from_raw_parts((*result).images, (*result).image_count);
            for view in views {
                assert!(!view.filename.is_null() && view.filename_len > 0);
                assert!(!view.bytes.is_null());
            }
            pdfmd_result_free(result);
        }
    }

    #[test]
    fn convert_surfaces_conversion_errors() {
        let bytes = b"not a pdf at all";
        unsafe {
            let result = pdfmd_convert(bytes.as_ptr(), bytes.len(), false, std::ptr::null());
            let (markdown, error) = parts(result);
            assert!(markdown.is_none());
            assert_eq!(error.unwrap(), crate::Error::NotPdf.to_string());
            pdfmd_result_free(result);
        }
    }

    #[test]
    fn convert_rejects_a_null_pointer_with_a_length() {
        unsafe {
            let result = pdfmd_convert(std::ptr::null(), 12, false, std::ptr::null());
            let (_, error) = parts(result);
            assert_eq!(error.unwrap(), "null input pointer");
            pdfmd_result_free(result);
        }
    }

    #[test]
    fn convert_treats_a_null_pointer_with_zero_length_as_empty_input() {
        unsafe {
            let result = pdfmd_convert(std::ptr::null(), 0, false, std::ptr::null());
            let (_, error) = parts(result);
            assert_eq!(error.unwrap(), crate::Error::NotPdf.to_string());
            pdfmd_result_free(result);
        }
    }

    #[test]
    fn convert_rejects_non_utf8_image_dir() {
        let bytes = fixture();
        let dir = std::ffi::CString::new(vec![0xff, 0xfe]).unwrap();
        unsafe {
            let result = pdfmd_convert(bytes.as_ptr(), bytes.len(), false, dir.as_ptr());
            let (_, error) = parts(result);
            assert_eq!(error.unwrap(), "image_dir is not valid UTF-8");
            pdfmd_result_free(result);
        }
    }

    #[test]
    fn result_free_ignores_null() {
        unsafe { pdfmd_result_free(std::ptr::null_mut()) };
    }
}
