//! PaddleOCR ONNX Runtime integration (desktop-api shared layer).
//!
//! Logic ported from the main crate's `src/desktop/paddle_ocr.rs` (PP-OCRv4 ONNX local OCR),
//! keeping the same preprocessing/inference/postprocessing pipeline; only the main crate's debug
//! probes and model-file-dependent unit tests are stripped. Runtime requires `onnxruntime.dll` to be
//! loadable (ort load-dynamic) and the model trio to be present in the directory resolved by resolve_models_dir().

use std::path::PathBuf;

use crate::vision::models::{
    resolve_models_dir, validate_ocr_models, PADDLE_DET_MODEL, PADDLE_DICT, PADDLE_REC_MODEL,
};

/// Ensure `ORT_DYLIB_PATH` points at a sane ONNX Runtime dylib **before** ort's
/// one-shot loader runs (its `OnceLock` latches the first resolution).
///
/// ort otherwise resolves the bare name `onnxruntime.dll` through the OS search
/// path, which can pick up a STALE copy (e.g. a v1.0 in System32) that loads but
/// hangs session creation forever (documented in nuphus-mcp's build.rs). This
/// bites cargo test binaries in particular: they live in `target/<profile>/deps/`
/// while build scripts place the dylib one level up in `target/<profile>/`.
///
/// No-op when `ORT_DYLIB_PATH` is already set or no candidate exists (falls back
/// to ort's default resolution, e.g. a properly deployed exe-adjacent dylib).
pub fn ensure_ort_dylib() {
    if std::env::var_os("ORT_DYLIB_PATH").is_some() {
        return;
    }
    #[cfg(target_os = "windows")]
    const DYLIB_NAME: &str = "onnxruntime.dll";
    #[cfg(target_os = "linux")]
    const DYLIB_NAME: &str = "libonnxruntime.so";
    #[cfg(target_os = "macos")]
    const DYLIB_NAME: &str = "libonnxruntime.dylib";

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let mut dirs = exe
        .parent()
        .map(|p| p.to_path_buf())
        .into_iter()
        .collect::<Vec<_>>();
    // Cargo puts test binaries in deps/ one level below the dylib.
    if let Some(parent) = exe.parent().and_then(|p| p.parent()) {
        dirs.push(parent.to_path_buf());
    }
    for dir in dirs {
        let candidate = dir.join(DYLIB_NAME);
        if candidate.exists() {
            std::env::set_var("ORT_DYLIB_PATH", &candidate);
            return;
        }
    }
}

/// OCR text block (with position)
#[derive(Debug, Clone)]
pub struct OcrBlock {
    pub text: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// PaddleOCR engine: text detection (det) → per-box recognition (rec) → CTC decoding.
pub struct PaddleOcr {
    det_session: ort::session::Session,
    rec_session: ort::session::Session,
    char_dict: Vec<String>,
    /// Models directory this instance was built from — the process-wide singleton
    /// rebuilds when the resolved directory changes (e.g. NUPHUS_MODELS_DIR switch).
    models_dir: PathBuf,
}

/// Process-wide shared engine (P1: per-call construction re-read the 6623-line
/// dictionary and re-committed both ONNX sessions on every single perceive).
/// Inference is serialized by this mutex — ORT sessions are not re-entrant.
static SHARED_OCR: std::sync::Mutex<Option<PaddleOcr>> = std::sync::Mutex::new(None);

impl PaddleOcr {
    /// Run `f` with the process-wide shared engine, initializing it on first use
    /// and rebuilding it when the resolved models directory changed.
    pub fn with_shared<R>(
        f: impl FnOnce(&mut PaddleOcr) -> Result<R, String>,
    ) -> Result<R, String> {
        let mut guard = SHARED_OCR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Self::models_dir()?;
        let rebuild = match guard.as_ref() {
            Some(ocr) => ocr.models_dir != dir,
            None => true,
        };
        if rebuild {
            if guard.is_some() {
                tracing::info!("[paddle-ocr] models directory changed, rebuilding engine");
            }
            *guard = Some(Self::new()?);
        }
        f(guard.as_mut().expect("engine initialized above"))
    }

    /// Initialize the engine: load the detection model, recognition model, and character dictionary.
    /// Missing models return a clear error (including the missing paths, for download guidance).
    pub fn new() -> Result<Self, String> {
        ensure_ort_dylib();
        let models_dir = Self::models_dir()?;
        validate_ocr_models(&models_dir)?;

        let det_path = models_dir.join(PADDLE_DET_MODEL);
        let rec_path = models_dir.join(PADDLE_REC_MODEL);
        let dict_path = models_dir.join(PADDLE_DICT);

        // Load ONNX sessions
        let det_session = ort::session::Session::builder()
            .map_err(|e| format!("failed to create detection session builder: {e}"))?
            .commit_from_file(det_path)
            .map_err(|e| format!("failed to load detection model: {e}"))?;

        let rec_session = ort::session::Session::builder()
            .map_err(|e| format!("failed to create recognition session builder: {e}"))?
            .commit_from_file(rec_path)
            .map_err(|e| format!("failed to load recognition model: {e}"))?;

        // Load the character dictionary
        let dict_content = std::fs::read_to_string(&dict_path)
            .map_err(|e| format!("failed to read dictionary: {e}"))?;
        let char_dict: Vec<String> = dict_content.lines().map(|s| s.to_owned()).collect();

        if char_dict.len() < 100 {
            return Err(format!(
                "invalid dictionary content: only {} characters (expected ~6623)",
                char_dict.len()
            ));
        }

        Ok(Self {
            det_session,
            rec_session,
            char_dict,
            models_dir,
        })
    }

    /// OCR an image file, returning text blocks with positions.
    pub fn ocr_with_boxes(&mut self, image_path: &str) -> Result<Vec<OcrBlock>, String> {
        let img = image::open(image_path)
            .map_err(|e| format!("failed to open image {image_path}: {e}"))?
            .to_rgb8();
        self.ocr_image_with_boxes(&img)
    }

    /// OCR an in-memory RGB image, returning text blocks with positions (no disk writes).
    pub fn ocr_image_with_boxes(&mut self, img: &image::RgbImage) -> Result<Vec<OcrBlock>, String> {
        // Stage 1: detect text boxes
        let boxes = self.detect(img)?;

        if boxes.is_empty() {
            return Ok(vec![]);
        }

        // Stage 2: recognize text in each box
        let (img_w, img_h) = (img.width(), img.height());
        let mut results: Vec<OcrBlock> = Vec::new();
        for &(x1, y1, x2, y2) in &boxes {
            let (cx1, cy1, cx2, cy2) = Self::expand_box((x1, y1, x2, y2), img_w, img_h);
            let (cw, ch) = (cx2.saturating_sub(cx1), cy2.saturating_sub(cy1));
            // Degenerate detection boxes (zero width/height) would panic in
            // crop_imm — skip them instead of crashing the pipeline.
            if cw == 0 || ch == 0 {
                continue;
            }
            let crop = image::imageops::crop_imm(img, cx1, cy1, cw, ch);
            let text = self.recognize(crop.to_image())?;
            if !text.is_empty() {
                results.push(OcrBlock {
                    text,
                    x: x1 as i32,
                    y: y1 as i32,
                    w: (x2.saturating_sub(x1)) as i32,
                    h: (y2.saturating_sub(y1)) as i32,
                });
            }
        }

        results.sort_by(|a, b| a.y.cmp(&b.y).then(a.x.cmp(&b.x)));
        Ok(results)
    }

    /// OCR an image file, returning plain text (same pipeline as ocr_with_boxes).
    pub fn ocr(&mut self, image_path: &str) -> Result<String, String> {
        let img = image::open(image_path)
            .map_err(|e| format!("failed to open image {image_path}: {e}"))?
            .to_rgb8();
        self.ocr_image(&img)
    }

    /// OCR an in-memory RGB image, returning plain text.
    pub fn ocr_image(&mut self, img: &image::RgbImage) -> Result<String, String> {
        let blocks = self.ocr_image_with_boxes(img)?;
        Ok(blocks
            .into_iter()
            .map(|b| b.text)
            .collect::<Vec<_>>()
            .join("\n"))
    }

    // ─── Text detection ───────────────────────────────────────────

    /// Text detection: returns [(x1, y1, x2, y2), ...]
    fn detect(&mut self, img: &image::RgbImage) -> Result<Vec<(u32, u32, u32, u32)>, String> {
        let (img_w, img_h) = (img.width() as f32, img.height() as f32);

        // Preprocessing
        let (input, scale, pad_x, pad_y) = Self::preprocess_det(img);

        // Inference
        let input_value = ort::value::TensorRef::from_array_view(input.view())
            .map_err(|e| format!("failed to create detection input: {e}"))?;

        let outputs = self
            .det_session
            .run(ort::inputs!["x" => input_value])
            .map_err(|e| format!("detection inference failed: {e}"))?;

        // Postprocessing
        Self::postprocess_det(&outputs, img_w, img_h, scale, pad_x, pad_y)
    }

    /// Detection preprocessing: resize + normalize → NCHW f32 tensor
    fn preprocess_det(img: &image::RgbImage) -> (ndarray::Array4<f32>, f32, u32, u32) {
        let (w, h) = (img.width(), img.height());

        let max_size = 960.0;
        let limit_side_len = if w.max(h) as f32 > max_size {
            max_size
        } else {
            w.max(h) as f32
        };
        let ratio = if w > h {
            limit_side_len / w as f32
        } else {
            limit_side_len / h as f32
        };

        let new_w = ((w as f32 * ratio) as u32).max(32);
        let new_h = ((h as f32 * ratio) as u32).max(32);

        let pad_w = (new_w.div_ceil(32) * 32) - new_w;
        let pad_h = (new_h.div_ceil(32) * 32) - new_h;

        let resized =
            image::imageops::resize(img, new_w, new_h, image::imageops::FilterType::Triangle);

        let out_h = (new_h + pad_h) as usize;
        let out_w = (new_w + pad_w) as usize;

        // Build the CHW-format Array4 directly
        let mean = [0.485f32, 0.456, 0.406];
        let std = [0.229f32, 0.224, 0.225];

        let mut array = ndarray::Array4::<f32>::zeros((1, 3, out_h, out_w));

        for y in 0..new_h as usize {
            for x in 0..new_w as usize {
                let p = resized.get_pixel(x as u32, y as u32);
                // PaddleOCR uses RGB channel order (PIL default / cv2 BGR2RGB)
                // the image crate returns RGB, use directly without reordering
                let rgb = [0usize, 1, 2];
                for c in 0..3 {
                    let val = p[rgb[c]] as f32 / 255.0;
                    let normalized = (val - mean[c]) / std[c];
                    array[[0, c, y, x]] = normalized;
                }
            }
        }
        // pad region is already 0

        (array, ratio, pad_w, pad_h)
    }

    /// Detection postprocessing: extract text bounding boxes from the model output
    fn postprocess_det(
        outputs: &ort::session::SessionOutputs,
        img_w: f32,
        img_h: f32,
        scale: f32,
        pad_w: u32,
        pad_h: u32,
    ) -> Result<Vec<(u32, u32, u32, u32)>, String> {
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("failed to extract detection output: {e}"))?;

        let ndim = shape.len();
        let dims: Vec<usize> = (0..ndim).map(|i| shape[i] as usize).collect();

        // Case 1: output is (N, 6) format — [cls, score, x1, y1, x2, y2]
        if ndim == 2 && dims[1] == 6 {
            let num_boxes = dims[0];
            let mut boxes = Vec::new();
            for i in 0..num_boxes {
                let offset = i * 6;
                let score = data[offset + 1];
                if score < 0.5 {
                    continue;
                }
                let (x1, y1, x2, y2) = (
                    data[offset + 2],
                    data[offset + 3],
                    data[offset + 4],
                    data[offset + 5],
                );
                let (x1, y1, x2, y2) = if x1 <= 1.0 && x2 <= 1.0 && y1 <= 1.0 && y2 <= 1.0 {
                    (
                        (x1 * img_w) as u32,
                        (y1 * img_h) as u32,
                        (x2 * img_w) as u32,
                        (y2 * img_h) as u32,
                    )
                } else {
                    (
                        (x1 / scale).max(0.0) as u32,
                        (y1 / scale).max(0.0) as u32,
                        (x2 / scale).max(0.0) as u32,
                        (y2 / scale).max(0.0) as u32,
                    )
                };
                boxes.push((x1, y1, x2, y2));
            }
            return Ok(boxes);
        }

        // Case 2: output is a probability map (1, 1, H, W)
        if ndim == 4 && dims[1] == 1 {
            let h = dims[2] as u32;
            let w = dims[3] as u32;
            let threshold: f32 = 0.3;

            let mut bitmap = vec![false; (h * w) as usize];
            for y in 0..h {
                for x in 0..w {
                    let v = data[(y * w + x) as usize];
                    if v > threshold {
                        bitmap[(y * w + x) as usize] = true;
                    }
                }
            }

            let min_area = 100.0;
            let boxes = Self::find_boxes_from_bitmap(&bitmap, w, h, min_area, scale, pad_w, pad_h);
            return Ok(boxes);
        }

        Err(format!(
            "unsupported detection output dimensions: {:?}, expected (N,6) or (1,1,H,W)",
            dims
        ))
    }

    /// Extract text boxes from a binary bitmap (simplified connected-component detection)
    fn find_boxes_from_bitmap(
        bitmap: &[bool],
        w: u32,
        h: u32,
        min_area: f32,
        scale: f32,
        _pad_w: u32,
        _pad_h: u32,
    ) -> Vec<(u32, u32, u32, u32)> {
        let mut visited = vec![false; bitmap.len()];
        let mut boxes = Vec::new();
        let step = 4u32;

        for y in (0..h).step_by(step as usize) {
            for x in (0..w).step_by(step as usize) {
                let idx = (y * w + x) as usize;
                if !bitmap[idx] || visited[idx] {
                    continue;
                }

                let (mut min_x, mut max_x) = (x, x);
                let (mut min_y, mut max_y) = (y, y);
                let mut stack = vec![(x, y)];
                visited[idx] = true;
                let mut area = 0u32;

                while let Some((cx, cy)) = stack.pop() {
                    area += 1;
                    min_x = min_x.min(cx);
                    max_x = max_x.max(cx);
                    min_y = min_y.min(cy);
                    max_y = max_y.max(cy);

                    for (nx, ny) in [
                        (cx.wrapping_sub(1), cy),
                        (cx + 1, cy),
                        (cx, cy.wrapping_sub(1)),
                        (cx, cy + 1),
                    ] {
                        if nx < w && ny < h {
                            let ni = (ny * w + nx) as usize;
                            if bitmap[ni] && !visited[ni] {
                                visited[ni] = true;
                                stack.push((nx, ny));
                            }
                        }
                    }
                }

                if area as f32 >= min_area {
                    let orig_x1 = (min_x as f32 / scale).max(0.0) as u32;
                    let orig_y1 = (min_y as f32 / scale).max(0.0) as u32;
                    let orig_x2 = (max_x as f32 / scale).max(0.0) as u32;
                    let orig_y2 = (max_y as f32 / scale).max(0.0) as u32;
                    boxes.push((orig_x1, orig_y1, orig_x2, orig_y2));
                }
            }
        }

        boxes
    }

    /// Expand a text box (improves recognition accuracy)
    fn expand_box(
        (x1, y1, x2, y2): (u32, u32, u32, u32),
        img_w: u32,
        img_h: u32,
    ) -> (u32, u32, u32, u32) {
        let bw = (x2.saturating_sub(x1)) as f32;
        let bh = (y2.saturating_sub(y1)) as f32;
        if bw < 1.0 || bh < 1.0 {
            return (x1, y1, x2, y2);
        }
        let margin_x = (bw * 0.05) as u32;
        let margin_y = (bh * 0.10) as u32;
        (
            x1.saturating_sub(margin_x),
            y1.saturating_sub(margin_y),
            (x2 + margin_x).min(img_w),
            (y2 + margin_y).min(img_h),
        )
    }

    // ─── Text recognition ───────────────────────────────────────────

    /// Recognize a single text region: preprocessing → inference → CTC decoding
    fn recognize(&mut self, crop: image::RgbImage) -> Result<String, String> {
        let input = Self::preprocess_rec(&crop);
        let input_value = ort::value::TensorRef::from_array_view(input.view())
            .map_err(|e| format!("failed to create recognition input: {e}"))?;

        let outputs = self
            .rec_session
            .run(ort::inputs!["x" => input_value])
            .map_err(|e| format!("recognition inference failed: {e}"))?;

        Self::ctc_decode(&self.char_dict, &outputs)
    }

    /// Recognition preprocessing: resize → normalize → NCHW (height 48, width 320, zero-pad on the right)
    fn preprocess_rec(img: &image::RgbImage) -> ndarray::Array4<f32> {
        let (w, h) = (img.width(), img.height());
        let target_h = 48u32;
        let target_w = 320u32;

        let ratio = target_h as f32 / h as f32;
        let new_w = (w as f32 * ratio) as u32;
        let resized = image::imageops::resize(
            img,
            new_w.min(target_w),
            target_h,
            image::imageops::FilterType::Triangle,
        );

        // Zero-pad the right side to target_w (PaddleOCR is trained with 320 width)
        let mean = [0.5f32, 0.5, 0.5];
        let std = [0.5f32, 0.5, 0.5];

        let mut array = ndarray::Array4::<f32>::zeros((1, 3, target_h as usize, target_w as usize));

        for y in 0..target_h as usize {
            for x in 0..resized.width() as usize {
                let p = resized.get_pixel(x as u32, y as u32);
                // PaddleOCR uses RGB channel order
                let rgb = [0usize, 1, 2];
                for c in 0..3 {
                    let val = p[rgb[c]] as f32 / 255.0;
                    let normalized = (val - mean[c]) / std[c];
                    array[[0, c, y, x]] = normalized;
                }
            }
        }

        array
    }

    /// CTC decoding: decode text from the recognition model output
    fn ctc_decode(
        char_dict: &[String],
        outputs: &ort::session::SessionOutputs,
    ) -> Result<String, String> {
        let (shape, data) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("failed to extract recognition output: {e}"))?;

        let ndim = shape.len();
        if ndim != 3 {
            return Err(format!(
                "unsupported recognition output dimensions: {:?}",
                (0..ndim).map(|i| shape[i]).collect::<Vec<_>>()
            ));
        }

        let (seq_len, num_classes) = (shape[1] as usize, shape[2] as usize);
        // PaddleOCR training conventions:
        // - class 0  = CTC blank (skip)
        // - class 1..=char_dict.len() = dictionary characters (dict[0..])
        // - class num_classes-1 = space character (keep, do not skip!)
        let blank_id = 0usize;
        let space_id = num_classes.saturating_sub(1);

        let mut prev = blank_id;
        let mut chars = Vec::new();

        for t in 0..seq_len {
            let mut max_val = f32::NEG_INFINITY;
            let mut max_idx = blank_id;
            let offset = t * num_classes;
            for c in 0..num_classes {
                let val = data[offset + c];
                if val > max_val {
                    max_val = val;
                    max_idx = c;
                }
            }

            if max_idx != blank_id && max_idx != prev {
                if max_idx == space_id {
                    chars.push(" ".to_string());
                } else if max_idx >= 1 && max_idx <= char_dict.len() {
                    chars.push(char_dict[max_idx - 1].clone());
                }
            }
            prev = max_idx;
        }

        Ok(chars.join(""))
    }

    // ─── Utility functions ───────────────────────────────────────────

    /// Get the model directory — delegates to the public resolve_models_dir()
    fn models_dir() -> Result<PathBuf, String> {
        resolve_models_dir().ok_or_else(|| {
            "model directory not found; set the NUPHUS_MODELS_DIR environment variable or ensure the model files are in the correct location"
                .to_string()
        })
    }
}

/// Trial-load an ONNX file into an ORT session — the strongest integrity check a
/// downloaded model can get before being trusted as valid (used by nuphus-mcp's
/// model downloader after the byte-level checks).
pub fn onnx_session_loadable(path: &std::path::Path) -> Result<(), String> {
    ensure_ort_dylib();
    ort::session::Session::builder()
        .map_err(|e| format!("failed to create session builder: {e}"))?
        .commit_from_file(path)
        .map_err(|e| format!("ONNX trial load failed for {}: {e}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Garbage bytes must not pass the ORT trial load (download integrity gate).
    #[test]
    fn onnx_trial_load_rejects_garbage() {
        let tmp = std::env::temp_dir().join("nuphus_desktop_api_onnx_garbage.onnx");
        std::fs::write(&tmp, b"<html>404 not found</html>").unwrap();
        let result = onnx_session_loadable(&tmp);
        let _ = std::fs::remove_file(&tmp);
        let err = result.expect_err("garbage must be rejected");
        assert!(err.contains("ONNX trial load failed"), "err: {err}");
    }

    /// The shared engine must fail honestly (not panic, not poison the singleton)
    /// when the models directory has no valid models — and must recover once the
    /// directory changes. Exercises the singleton rebuild decision.
    ///
    /// Serializes with any other test touching NUPHUS_MODELS_DIR via a file-local lock.
    #[test]
    fn with_shared_rebuilds_on_models_dir_change() {
        static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _lock = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        let saved = std::env::var("NUPHUS_MODELS_DIR").ok();
        let empty = std::env::temp_dir().join("nuphus_desktop_api_empty_models");
        std::fs::create_dir_all(&empty).unwrap();
        std::env::set_var("NUPHUS_MODELS_DIR", &empty);

        // Empty dir → honest error, singleton stays uninitialized.
        let result = PaddleOcr::with_shared(|_| Ok(()));
        let err = result.expect_err("empty models dir must fail");
        assert!(
            err.contains(PADDLE_DET_MODEL) || err.contains("model"),
            "error names missing models: {err}"
        );

        // Point back to the real directory (if this machine has one): the
        // singleton must rebuild against the new directory instead of latching
        // the failure.
        match saved {
            Some(v) => std::env::set_var("NUPHUS_MODELS_DIR", v),
            None => std::env::remove_var("NUPHUS_MODELS_DIR"),
        }
        if resolve_models_dir()
            .map(|d| validate_ocr_models(&d).is_ok())
            .unwrap_or(false)
        {
            PaddleOcr::with_shared(|_| Ok(())).expect("rebuild against real models dir");
        }
    }
}
