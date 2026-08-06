//! `desktop_perceive` local model management — downloads the PaddleOCR models and the YOLO icon
//! detection model automatically on first run (both are part of the same perceive pipeline).
//!
//! Model files land in the same location as the main app (`NUPHUS_MODELS_DIR` or `data_dir/Nuphus/models`),
//! resolved by `vision::models` in desktop-api. Download sources mirror the main crate's build.rs:
//! - Detection/recognition ONNX: `hf-mirror.com/SWHL/RapidOCR` (HuggingFace mirror),
//!   falling back to the official `huggingface.co` source on failure.
//! - Character dictionary: `gitee.com/paddlepaddle/PaddleOCR`.
//! - YOLO `icon_detect.onnx`: `onnx-community/OmniParser-icon_detect_640x640` ONNX export
//!   (`hf-mirror.com` first, `huggingface.co` fallback); the same I/O contract as the Nuphus
//!   exported model (`images [1,3,640,640]` → `output0 [1,5,8400]`).
//!
//! YOLO stays optional at runtime: if its download fails, perceive runs in OCR-only mode and
//! honestly reports YOLO as unavailable. Users can pin a custom source (e.g. a private mirror or
//! the full ~80MB OmniParser export) via `NUPHUS_MCP_YOLO_MODEL_URL`.

use std::path::PathBuf;
use std::time::Duration;

use desktop_api::vision::models::{
    models_dir_for_write, validate_ocr_models, yolo_model_available, PADDLE_OCR_FILES, YOLO_MODEL,
};

/// Model readiness status (used by desktop_perceive).
#[derive(Debug, Clone)]
pub struct ModelStatus {
    pub dir: PathBuf,
    /// Whether the PaddleOCR trio is complete (hard prerequisite for perceive)
    pub ocr_ready: bool,
    /// Whether the YOLO icon detection model is available (optional enhancement)
    pub yolo_available: bool,
    /// Files actually downloaded by this call
    pub downloaded: Vec<String>,
}

/// Candidate download sources per file (tried in order).
fn sources_for(file: &str) -> Vec<&'static str> {
    match file {
        "ch_PP-OCRv4_det.onnx" => vec![
            "https://hf-mirror.com/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx",
            "https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_det_infer.onnx",
        ],
        "ch_PP-OCRv4_rec.onnx" => vec![
            "https://hf-mirror.com/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_rec_infer.onnx",
            "https://huggingface.co/SWHL/RapidOCR/resolve/main/PP-OCRv4/ch_PP-OCRv4_rec_infer.onnx",
        ],
        "ch_PP-OCR_keys_v1.txt" => {
            vec!["https://gitee.com/paddlepaddle/PaddleOCR/raw/main/ppocr/utils/ppocr_keys_v1.txt"]
        }
        _ => vec![],
    }
}

/// Ensure local models are ready: auto-download the PaddleOCR files and the YOLO icon detection
/// model together on first run.
///
/// - OCR files all present or downloaded successfully → `Ok(status)` (`yolo_available` reported honestly).
/// - Any OCR file download fails → `Err` (clear error + manual download instructions), no panic.
/// - YOLO download failure is **not** fatal: perceive degrades to OCR-only and reports `yolo_available=false`.
/// - Setting `NUPHUS_MCP_NO_MODEL_DOWNLOAD=1` skips both downloads (fast-fail for restricted networks/CI:
///   existence-only check that returns a clear error).
pub async fn ensure_models() -> Result<ModelStatus, String> {
    let dir = models_dir_for_write()?;
    let mut downloaded: Vec<String> = Vec::new();

    let skip_download = std::env::var("NUPHUS_MCP_NO_MODEL_DOWNLOAD")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false);

    // Validate first, to avoid unnecessary requests when everything is already present.
    // One shared client covers the OCR trio and the YOLO model.
    let ocr_missing = validate_ocr_models(&dir).is_err();
    let yolo_missing = !yolo_model_available(&dir);
    if !skip_download && (ocr_missing || yolo_missing) {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|e| format!("build http client failed: {}", e))?;

        if ocr_missing {
            for file in PADDLE_OCR_FILES {
                let dest = dir.join(file);
                if dest.exists() {
                    continue;
                }
                let sources = sources_for(file);
                if sources.is_empty() {
                    continue;
                }
                match download_with_fallback(&client, &sources, &dest).await {
                    Ok(()) => {
                        downloaded.push(file.to_string());
                    }
                    Err(e) => {
                        return Err(format!(
                            "desktop_perceive model download failed for {file}: {e}\n\
                             Please download the following files manually into {}:\n  - ch_PP-OCRv4_det.onnx ← https://hf-mirror.com/SWHL/RapidOCR\n  - ch_PP-OCRv4_rec.onnx ← https://hf-mirror.com/SWHL/RapidOCR\n  - ch_PP-OCR_keys_v1.txt ← https://gitee.com/paddlepaddle/PaddleOCR",
                            dir.display()
                        ));
                    }
                }
            }
        }

        // YOLO icon detection model — same download pipeline as the OCR files, including the
        // size floor and the ONNX trial-load gate. A failure is downgraded, not fatal.
        if yolo_missing {
            let dest = dir.join(YOLO_MODEL);
            match download_with_fallback(&client, &yolo_sources(), &dest).await {
                Ok(()) => downloaded.push(YOLO_MODEL.to_string()),
                Err(e) => tracing::warn!(
                    "[models] YOLO model download failed, perceive continues in OCR-only mode: {e}"
                ),
            }
        }
    }

    // Final validation: OCR must be complete, otherwise report an error (with missing files)
    validate_ocr_models(&dir).map_err(|e| {
        format!(
            "{e}\nAutomatic download did not complete. Please check your network and retry, or place the model files manually in {}",
            dir.display()
        )
    })?;

    Ok(ModelStatus {
        dir: dir.clone(),
        ocr_ready: true,
        yolo_available: yolo_model_available(&dir),
        downloaded,
    })
}

/// YOLO `icon_detect.onnx` download sources (tried in order).
///
/// `NUPHUS_MCP_YOLO_MODEL_URL` overrides the default entirely — for a private mirror or the full
/// ~80MB OmniParser export. Default: onnx-community's OmniParser icon_detect 640x640 ONNX export,
/// which exposes the same I/O contract the YOLO runtime expects
/// (`images [1,3,640,640]` → `output0 [1,5,8400]`), via hf-mirror.com first (China-friendly),
/// huggingface.co fallback.
fn yolo_sources() -> Vec<String> {
    if let Ok(url) = std::env::var("NUPHUS_MCP_YOLO_MODEL_URL") {
        let url = url.trim().to_string();
        if !url.is_empty() {
            return vec![url];
        }
    }
    vec![
        "https://hf-mirror.com/onnx-community/OmniParser-icon_detect_640x640/resolve/main/onnx/model.onnx"
            .to_string(),
        "https://huggingface.co/onnx-community/OmniParser-icon_detect_640x640/resolve/main/onnx/model.onnx"
            .to_string(),
    ]
}

/// Try each candidate source in order; return Err if all fail.
async fn download_with_fallback<S: AsRef<str>>(
    client: &reqwest::Client,
    sources: &[S],
    dest: &std::path::Path,
) -> Result<(), String> {
    let mut last_err: Option<String> = None;
    for url in sources {
        match download_file(client, url.as_ref(), dest).await {
            Ok(()) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| "no source".to_string()))
}

/// Minimum accepted file size per model (bytes).
///
/// Guards against empty/truncated downloads that succeed with HTTP 200 but carry an
/// error page or a partial body. Real artifacts: det ~4.7 MB, rec ~10.8 MB,
/// dictionary ~26 KB (measured from the official RapidOCR/PaddleOCR sources), so
/// these floors can never reject a real artifact while being 10x stricter than a
/// generic 100 KB floor. On top of the floor, ONNX files must pass an ORT trial
/// load before they are trusted (see `download_file`).
fn min_expected_bytes(file: &str) -> u64 {
    match file {
        "ch_PP-OCRv4_det.onnx" | "ch_PP-OCRv4_rec.onnx" => 1_000_000,
        // icon_detect.onnx default source is ~12MB fp32; smallest legit variant is ~3.3MB
        // (int8), so 2MB rejects error pages while accepting any real export.
        "icon_detect.onnx" => 2_000_000,
        "ch_PP-OCR_keys_v1.txt" => 10_000,
        _ => 0,
    }
}

/// Optional per-file SHA-256 enforcement.
///
/// If `NUPHUS_MCP_MODEL_SHA256_<FILE>` is set, downloaded bytes must match the given
/// digest or the download is rejected. This lets operators pin downloads to a known-good
/// artifact (e.g. a trusted mirror) against supply-chain tampering. When unset, only the
/// size floor above applies.
fn expected_sha256(file: &str) -> Option<String> {
    let key = format!("NUPHUS_MCP_MODEL_SHA256_{file}");
    std::env::var(&key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Stream-download a single file to a temp path, verify integrity, then rename atomically
/// (avoid a partial or tampered file being treated as a valid model).
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    dest: &std::path::Path,
) -> Result<(), String> {
    // Unique temp name per process: two nuphus-mcp instances downloading the same
    // model used to interleave writes into one fixed `.part` file, producing a
    // permanently corrupt model. PID suffix isolates the scratch file; the final
    // atomic rename makes the last (complete) writer win.
    let tmp = dest.with_extension(format!("part.{}", std::process::id()));
    let result = download_file_inner(client, url, dest, &tmp).await;
    if result.is_err() {
        // No half-written scratch file may survive a failed download.
        let _ = tokio::fs::remove_file(&tmp).await;
    }
    result
}

async fn download_file_inner(
    client: &reqwest::Client,
    url: &str,
    dest: &std::path::Path,
    tmp: &std::path::Path,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("GET {url} failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("GET {url} -> HTTP {}", resp.status()));
    }

    let mut file = tokio::fs::File::create(tmp)
        .await
        .map_err(|e| format!("create temp file {} failed: {e}", tmp.display()))?;

    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("read response body failed: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("write temp file failed: {e}"))?;
    }
    file.flush()
        .await
        .map_err(|e| format!("flush failed: {e}"))?;
    drop(file);

    // Integrity check before the temp file can become a "valid" model.
    let file_name = dest.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let meta = tokio::fs::metadata(tmp)
        .await
        .map_err(|e| format!("stat temp file {} failed: {e}", tmp.display()))?;
    let floor = min_expected_bytes(file_name);
    if meta.len() < floor {
        return Err(format!(
            "download too small ({} bytes < {floor} floor), refusing to use as model: {file_name}",
            meta.len()
        ));
    }
    if let Some(expected) = expected_sha256(file_name) {
        let bytes = tokio::fs::read(tmp)
            .await
            .map_err(|e| format!("read temp file for hashing failed: {e}"))?;
        use sha2::{Digest, Sha256};
        let actual = hex::encode(Sha256::digest(&bytes));
        if !actual.eq_ignore_ascii_case(&expected) {
            return Err(format!(
                "model SHA-256 mismatch for {file_name}: got {actual}, expected {expected}. \
                 Fix NUPHUS_MCP_MODEL_SHA256_{file_name} or unset it to skip hashing"
            ));
        }
    }

    // Strongest integrity gate: an ONNX model must actually load into the ONNX
    // Runtime. A truncated-but-above-floor or content-corrupted file is rejected
    // here instead of permanently wedging OCR with a "present but broken" model.
    if file_name.ends_with(".onnx") {
        let tmp_owned = tmp.to_path_buf();
        tokio::task::spawn_blocking(move || {
            desktop_api::vision::paddle_ocr::onnx_session_loadable(&tmp_owned)
        })
        .await
        .map_err(|e| format!("onnx validation worker failed: {e}"))??;
    }

    std::fs::rename(tmp, dest).map_err(|e| format!("rename to {} failed: {e}", dest.display()))?;
    tracing::info!("[models] download complete: {} <- {}", dest.display(), url);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paddle_ocr_file_constants_cover_all_three() {
        assert_eq!(PADDLE_OCR_FILES.len(), 3);
        for f in PADDLE_OCR_FILES {
            assert!(!sources_for(f).is_empty(), "{} must have sources", f);
        }
    }

    #[test]
    fn unknown_file_has_no_source() {
        assert!(sources_for("nope.onnx").is_empty());
        assert_eq!(desktop_api::vision::models::YOLO_MODEL, "icon_detect.onnx");
    }

    #[test]
    fn size_floor_covers_known_models() {
        for f in PADDLE_OCR_FILES {
            assert!(min_expected_bytes(f) > 0, "{f} must have a size floor");
        }
        // YOLO is now auto-downloaded too, so it must carry a size floor as well.
        assert!(
            min_expected_bytes(desktop_api::vision::models::YOLO_MODEL) > 0,
            "icon_detect.onnx must have a size floor"
        );
        assert_eq!(min_expected_bytes("nope.onnx"), 0);
    }

    #[test]
    fn yolo_sources_default_and_override() {
        let _lock = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());

        // Override: NUPHUS_MCP_YOLO_MODEL_URL wins and replaces the defaults entirely.
        std::env::set_var(
            "NUPHUS_MCP_YOLO_MODEL_URL",
            "https://example.com/custom/icon_detect.onnx",
        );
        assert_eq!(
            yolo_sources(),
            vec!["https://example.com/custom/icon_detect.onnx".to_string()]
        );

        // Unset: default list, mirror first.
        std::env::remove_var("NUPHUS_MCP_YOLO_MODEL_URL");
        let defaults = yolo_sources();
        assert!(
            defaults.len() >= 2,
            "expected mirror + fallback: {defaults:?}"
        );
        assert!(
            defaults[0].starts_with("https://hf-mirror.com/"),
            "mirror must be tried first: {defaults:?}"
        );
        assert!(
            defaults.iter().any(|u| u.contains("huggingface.co")),
            "huggingface fallback expected: {defaults:?}"
        );
    }

    #[test]
    fn sha256_env_override_respected() {
        let _lock = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        const KEY: &str = "NUPHUS_MCP_MODEL_SHA256_ch_PP-OCRv4_det.onnx";
        std::env::set_var(KEY, "  abc  ");
        assert_eq!(
            expected_sha256("ch_PP-OCRv4_det.onnx"),
            Some("abc".to_string())
        );
        std::env::remove_var(KEY);
        assert_eq!(expected_sha256("ch_PP-OCRv4_det.onnx"), None);
    }
}
