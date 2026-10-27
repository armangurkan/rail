//! `attachment.open` checks the bytes against the declared type before it writes the copy the
//! system opener is handed (#107): a part declared `image/png` or `text/plain` whose bytes are
//! markup is refused in Rail's own words and nothing is written, while a save still takes it.

use std::path::PathBuf;

use super::attachments_tests::{
    attachment_calls, bytes_at, entries, fixture, list_part, nothing_written, open,
};
use crate::error::ErrorClass;
use crate::ops::{CONTENT_MISMATCH, ErrorCode};

/// An SVG that runs a script the moment a browser renders it.
const SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"/>";

/// A PNG's signature and the start of its header chunk.
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

/// The SVG declared `image/png` is fetched, refused and never written; the refusal quotes
/// neither the name nor the bytes.
#[tokio::test]
async fn an_svg_declared_as_a_png_is_refused_and_never_written() {
    let (h, _dest) = fixture().await;
    list_part(&h, "0.4", "photo<svg>.png", "image/png", 64);
    bytes_at(&h, "0.4", SVG.to_vec());
    let before = attachment_calls(&h);
    let error = open(&h, "0.4")
        .await
        .expect_err("markup under a PNG's type");
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(error.class, ErrorClass::Permanent);
    assert_eq!(error.message, CONTENT_MISMATCH);
    assert_eq!(error.detail, None, "no outside text: {error:?}");
    for quoted in ["svg", "photo", "onload", "png"] {
        assert!(!error.to_string().contains(quoted), "{error}");
    }
    assert_eq!(attachment_calls(&h), before + 1, "the bytes were fetched");
    nothing_written(&h.paths.attachments_dir());
}

/// HTML declared `text/plain`, after a few words, is refused too.
#[tokio::test]
async fn html_declared_as_text_is_refused() {
    let (h, _dest) = fixture().await;
    list_part(&h, "0.6", "notes.txt", "text/plain; charset=utf-8", 64);
    bytes_at(
        &h,
        "0.6",
        b"see below\n<html><script>alert(1)</script>".to_vec(),
    );
    let error = open(&h, "0.6").await.expect_err("HTML under text's type");
    assert_eq!(error.message, CONTENT_MISMATCH);
    nothing_written(&h.paths.attachments_dir());
}

/// The bytes a PNG really has open as before, under the type's own extension.
#[tokio::test]
async fn a_real_png_still_opens() {
    let (h, _dest) = fixture().await;
    list_part(&h, "0.4", "photo.png", "image/png", 16);
    bytes_at(&h, "0.4", PNG.to_vec());
    let copy = PathBuf::from(open(&h, "0.4").await.expect("a PNG opens").path);
    assert!(copy.ends_with("photo.png"), "{}", copy.display());
    assert_eq!(std::fs::read(&copy).unwrap(), PNG);
    assert_eq!(entries(&h.paths.attachments_dir()).len(), 1);
}
