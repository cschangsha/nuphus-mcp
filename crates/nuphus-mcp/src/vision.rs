//! `desktop_vision` — BYOK cloud vision understanding.
//!
//! Two provider backends, selected by [`VisionProvider`]:
//! - **OpenAI-compatible** (default): `POST {base_url}/chat/completions` with a base64 `image_url` content block.
//! - **Anthropic Messages** (native): `POST {base_url}/messages` with `x-api-key` + `anthropic-version` headers,
//!   native `image` source block (base64 + `media_type`).
//!
//! Provider is set explicitly via `NUPHUS_MCP_VISION_PROVIDER` or auto-inferred from the base URL host
//! (`anthropic` in the host → Anthropic native; otherwise OpenAI-compatible). This is the same protocol shape
//! as `src/desktop/vision_ocr.rs` in the main crate, but **fully independent**: it does not depend on the main
//! crate's config/registry system, reading env vars directly:
//!
//! | Env var | Required | Default | Description |
//! |---------|----------|---------|-------------|
//! | `NUPHUS_MCP_VISION_API_KEY` | ✅ | - | API key (missing → explicit error) |
//! | `NUPHUS_MCP_VISION_BASE_URL` | - | `https://api.openai.com/v1` | Provider base URL (Anthropic: `https://api.anthropic.com/v1`) |
//! | `NUPHUS_MCP_VISION_MODEL` | ✅ | - | Vision model ID (e.g. `gpt-4o-mini`, `qwen-vl-max`, `claude-sonnet-4-5`) |
//! | `NUPHUS_MCP_VISION_PROVIDER` | - | `auto` | `auto` \| `openai` \| `anthropic`; `auto` infers from the base URL host |
//! | `NUPHUS_MCP_VISION_MAX_TOKENS` | - | `1024` | Max output tokens (1..=32768). Default 1024 = the compatibility floor for Chinese OpenAI-compatible providers (e.g. Zhipu GLM-4V-Flash caps at 1024); raise it for text-heavy screenshots |
//!
//! When key/model are not configured, returns a clear error: no panic, no silent "fake green" degradation.

use serde_json::{json, Value};

/// Vision provider backend. After config resolution this is never [`VisionProvider::Auto`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisionProvider {
    /// Infer from the base URL host (`anthropic` in host → Anthropic native, otherwise OpenAI-compatible).
    Auto,
    /// OpenAI-compatible `/chat/completions` (works with OpenAI, Azure, Ollama, vLLM, MiniMax, Qwen, …).
    OpenAI,
    /// Anthropic native Messages API (`/messages`, `x-api-key` + `anthropic-version`).
    Anthropic,
}

impl VisionProvider {
    fn from_env_str(s: &str) -> Result<Self, String> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "openai" | "openai-compatible" => Ok(Self::OpenAI),
            "anthropic" | "claude" => Ok(Self::Anthropic),
            other => Err(format!(
                "NUPHUS_MCP_VISION_PROVIDER must be one of auto|openai|anthropic, got: {other}"
            )),
        }
    }

    /// Resolve `Auto` against a base URL. `anthropic` anywhere in the host → Anthropic native
    /// (this also covers Anthropic-compatible third-party gateways, e.g. `api.minimax.io/anthropic`).
    fn resolve(self, base_url: &str) -> Self {
        match self {
            Self::Auto => {
                if base_url.to_ascii_lowercase().contains("anthropic") {
                    Self::Anthropic
                } else {
                    Self::OpenAI
                }
            }
            other => other,
        }
    }

    /// Human label for error messages / debugging.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::OpenAI => "OpenAI-compatible",
            Self::Anthropic => "Anthropic",
        }
    }
}

/// BYOK vision config (parsed from environment variables).
#[derive(Debug, Clone)]
pub struct VisionConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    /// Max output tokens. Default 1024 (Zhipu GLM-4V-Flash caps at 1024); override via `NUPHUS_MCP_VISION_MAX_TOKENS`.
    pub max_tokens: u32,
    /// Resolved provider — never [`VisionProvider::Auto`] after [`VisionConfig::from_env`].
    pub provider: VisionProvider,
}

impl VisionConfig {
    /// Read config from environment variables. Returns a clear error for missing required fields.
    pub fn from_env() -> Result<Self, String> {
        let api_key = std::env::var("NUPHUS_MCP_VISION_API_KEY")
            .map_err(|_| "NUPHUS_MCP_VISION_API_KEY required: set this environment variable to your vision model API key (BYOK)".to_string())?;
        if api_key.trim().is_empty() {
            return Err("NUPHUS_MCP_VISION_API_KEY required: value must not be empty".to_string());
        }

        let model = std::env::var("NUPHUS_MCP_VISION_MODEL")
            .map_err(|_| "NUPHUS_MCP_VISION_MODEL required: set the vision model id (e.g. gpt-4o-mini, qwen-vl-max, claude-sonnet-4-5)".to_string())?;
        if model.trim().is_empty() {
            return Err("NUPHUS_MCP_VISION_MODEL required: value must not be empty".to_string());
        }

        let base_url = std::env::var("NUPHUS_MCP_VISION_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let base_url = base_url.trim_end_matches('/').to_string();

        // Enforce HTTPS: screenshots sent to a remote endpoint must not leak over
        // plaintext. Loopback (localhost/127.0.0.1) is allowed over http for local
        // test endpoints (e.g. Ollama).
        let is_loopback = match reqwest::Url::parse(&base_url) {
            Ok(u) => matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "::1")),
            Err(_) => false,
        };
        if base_url.starts_with("http://") && !is_loopback {
            return Err(format!(
                "NUPHUS_MCP_VISION_BASE_URL must use https (plain http is only allowed for localhost/127.0.0.1 test endpoints): {base_url}"
            ));
        }

        let provider = match std::env::var("NUPHUS_MCP_VISION_PROVIDER") {
            Ok(s) => VisionProvider::from_env_str(&s)?,
            Err(_) => VisionProvider::Auto,
        }
        .resolve(&base_url);

        // Max output tokens: default 1024 (the compatibility floor for Chinese OpenAI-compatible
        // providers — Zhipu GLM-4V-Flash rejects anything above 1024 with a 400). Configurable so
        // text-heavy screenshots can request more from providers that allow it (Claude, GPT-4o, …).
        let max_tokens = match std::env::var("NUPHUS_MCP_VISION_MAX_TOKENS") {
            Ok(s) => s.trim().parse::<u32>().map_err(|_| {
                format!("NUPHUS_MCP_VISION_MAX_TOKENS must be a positive integer, got: {s}")
            })?,
            Err(_) => 1024,
        };
        if max_tokens == 0 || max_tokens > 32768 {
            return Err(format!(
                "NUPHUS_MCP_VISION_MAX_TOKENS must be in 1..=32768, got: {max_tokens}"
            ));
        }

        Ok(Self {
            api_key,
            base_url,
            model,
            max_tokens,
            provider,
        })
    }
}

/// Default prompt (used when no prompt parameter is given; English to be compatible with any vision model).
pub const DEFAULT_PROMPT: &str =
    "Describe the contents of this image in detail. If it contains readable text, output the text verbatim.";

/// OpenAI-compatible request: (url, body). Base64 image as an `image_url` data-URL content block.
fn openai_request(
    model: &str,
    base_url: &str,
    mime_type: &str,
    base64_image: &str,
    prompt: &str,
    max_tokens: u32,
) -> (String, Value) {
    let data_url = format!("data:{mime_type};base64,{base64_image}");
    let body = json!({
        "model": model,
        "messages": [
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": prompt },
                    {
                        "type": "image_url",
                        "image_url": { "url": data_url, "detail": "high" }
                    }
                ]
            }
        ],
        "max_tokens": max_tokens
    });
    (format!("{base_url}/chat/completions"), body)
}

/// Anthropic native Messages request: (url, body). Base64 image as an `image` source block.
fn anthropic_request(
    model: &str,
    base_url: &str,
    mime_type: &str,
    base64_image: &str,
    prompt: &str,
    max_tokens: u32,
) -> (String, Value) {
    let body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": [
            {
                "role": "user",
                "content": [
                    {
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": mime_type,
                            "data": base64_image
                        }
                    },
                    { "type": "text", "text": prompt }
                ]
            }
        ]
    });
    (format!("{base_url}/messages"), body)
}

/// Auth (and version) headers for the selected provider.
fn provider_headers(provider: VisionProvider, api_key: &str) -> Vec<(String, String)> {
    match provider {
        VisionProvider::Anthropic => vec![
            ("x-api-key".into(), api_key.into()),
            ("anthropic-version".into(), "2023-06-01".into()),
        ],
        VisionProvider::OpenAI | VisionProvider::Auto => {
            vec![("authorization".into(), format!("Bearer {api_key}"))]
        }
    }
}

/// OpenAI-compatible response: `choices[0].message.content` (string).
fn parse_openai_response(resp: &Value) -> Result<String, String> {
    let text = resp["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("vision API returned empty content".to_string());
    }
    Ok(text)
}

/// Anthropic native response: join all `content[*]` text blocks.
fn parse_anthropic_response(resp: &Value) -> Result<String, String> {
    let text = resp["content"]
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"] == "text")
                .filter_map(|b| b["text"].as_str())
                .collect::<String>()
        })
        .unwrap_or_default()
        .trim()
        .to_string();
    if text.is_empty() {
        return Err("vision API returned empty content".to_string());
    }
    Ok(text)
}

/// Run vision understanding on an image, returning the model's text reply.
///
/// - `image_path`: local image path (BMP/PNG both accepted; BMP is auto-converted to PNG before sending).
/// - `prompt`: optional prompt, defaults to [`DEFAULT_PROMPT`].
pub async fn vision_image(image_path: &str, prompt: Option<&str>) -> Result<String, String> {
    let config = VisionConfig::from_env()?;

    // 1. Read image → convert to PNG (LLM APIs generally do not accept image/bmp)
    let image_bytes = std::fs::read(image_path).map_err(|e| format!("read image failed: {}", e))?;
    let (mime_type, final_bytes) = if image_path.to_lowercase().ends_with(".png") {
        ("image/png", image_bytes)
    } else {
        let img = image::load_from_memory(&image_bytes)
            .map_err(|e| format!("decode image failed: {}", e))?;
        let mut png_buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png_buf, image::ImageFormat::Png)
            .map_err(|e| format!("convert to PNG failed: {}", e))?;
        ("image/png", png_buf.into_inner())
    };

    use base64::Engine;
    let base64_image = base64::engine::general_purpose::STANDARD.encode(&final_bytes);

    // 2. Build provider request
    let prompt_text = prompt.filter(|p| !p.is_empty()).unwrap_or(DEFAULT_PROMPT);
    let (url, body) = if config.provider == VisionProvider::Anthropic {
        anthropic_request(
            &config.model,
            &config.base_url,
            mime_type,
            &base64_image,
            prompt_text,
            config.max_tokens,
        )
    } else {
        openai_request(
            &config.model,
            &config.base_url,
            mime_type,
            &base64_image,
            prompt_text,
            config.max_tokens,
        )
    };

    // 3. POST
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("build http client failed: {}", e))?;

    let mut request = client.post(&url).header("Content-Type", "application/json");
    for (k, v) in provider_headers(config.provider, &config.api_key) {
        request = request.header(k, v);
    }
    let response = request
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("vision API request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let err_body = response.text().await.unwrap_or_default();
        return Err(format!(
            "vision API ({}) returned HTTP {status}: {err_body}",
            config.provider.label()
        ));
    }

    // 4. Parse response
    let resp_json: Value = response
        .json()
        .await
        .map_err(|e| format!("parse vision API response failed: {}", e))?;

    if config.provider == VisionProvider::Anthropic {
        parse_anthropic_response(&resp_json)
    } else {
        parse_openai_response(&resp_json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Env var mutations are global within the test process; serialize to avoid parallel tests polluting each other.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    }

    const VARS: [&str; 5] = [
        "NUPHUS_MCP_VISION_API_KEY",
        "NUPHUS_MCP_VISION_BASE_URL",
        "NUPHUS_MCP_VISION_MODEL",
        "NUPHUS_MCP_VISION_PROVIDER",
        "NUPHUS_MCP_VISION_MAX_TOKENS",
    ];

    /// Saves and clears the vision vars; restores them when the test ends.
    struct EnvGuard {
        saved: Vec<(String, Option<String>)>,
    }
    impl EnvGuard {
        fn clear() -> Self {
            let saved = VARS
                .iter()
                .map(|k| (k.to_string(), std::env::var(k).ok()))
                .collect();
            for k in VARS {
                std::env::remove_var(k);
            }
            EnvGuard { saved }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    fn key_model() {
        std::env::set_var("NUPHUS_MCP_VISION_API_KEY", "sk-test");
        std::env::set_var("NUPHUS_MCP_VISION_MODEL", "gpt-4o-mini");
    }

    #[test]
    fn missing_api_key_returns_clear_error() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        std::env::set_var("NUPHUS_MCP_VISION_MODEL", "gpt-4o-mini");
        let err = VisionConfig::from_env().expect_err("key missing must fail");
        assert!(
            err.contains("NUPHUS_MCP_VISION_API_KEY"),
            "error must name the missing env var: {err}"
        );
    }

    #[test]
    fn missing_model_returns_clear_error() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        std::env::set_var("NUPHUS_MCP_VISION_API_KEY", "sk-test");
        let err = VisionConfig::from_env().expect_err("model missing must fail");
        assert!(
            err.contains("NUPHUS_MCP_VISION_MODEL"),
            "error must name the missing env var: {err}"
        );
    }

    #[test]
    fn full_config_parses_with_defaults() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        let cfg = VisionConfig::from_env().expect("full config ok");
        assert_eq!(cfg.api_key, "sk-test");
        assert_eq!(cfg.model, "gpt-4o-mini");
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.provider, VisionProvider::OpenAI);
        assert_eq!(
            cfg.max_tokens, 1024,
            "default must be the 1024 compat floor"
        );
    }

    #[test]
    fn remote_http_base_url_rejected() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        std::env::set_var("NUPHUS_MCP_VISION_BASE_URL", "http://api.example.com/v1");
        let err = VisionConfig::from_env().expect_err("remote http must fail");
        assert!(err.contains("https"), "error must mention https: {err}");
    }

    #[test]
    fn localhost_http_base_url_allowed() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        std::env::set_var("NUPHUS_MCP_VISION_BASE_URL", "http://127.0.0.1:11434/v1");
        let cfg = VisionConfig::from_env().expect("loopback http ok");
        assert_eq!(cfg.base_url, "http://127.0.0.1:11434/v1");
    }

    #[test]
    fn base_url_trailing_slash_is_normalized() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        std::env::set_var("NUPHUS_MCP_VISION_BASE_URL", "https://example.com/v1/");
        let cfg = VisionConfig::from_env().expect("ok");
        assert_eq!(cfg.base_url, "https://example.com/v1");
    }

    // ---- Provider selection ----

    #[test]
    fn auto_detects_anthropic_from_base_url() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        std::env::set_var("NUPHUS_MCP_VISION_API_KEY", "sk-test");
        std::env::set_var("NUPHUS_MCP_VISION_MODEL", "claude-sonnet-4-5");
        std::env::set_var("NUPHUS_MCP_VISION_BASE_URL", "https://api.anthropic.com/v1");
        let cfg = VisionConfig::from_env().expect("ok");
        assert_eq!(cfg.provider, VisionProvider::Anthropic);
    }

    #[test]
    fn auto_detects_openai_for_default_base_url() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        let cfg = VisionConfig::from_env().expect("ok");
        assert_eq!(cfg.provider, VisionProvider::OpenAI);
    }

    #[test]
    fn explicit_provider_anthropic_wins_over_openai_base_url() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        std::env::set_var("NUPHUS_MCP_VISION_BASE_URL", "https://api.openai.com/v1");
        std::env::set_var("NUPHUS_MCP_VISION_PROVIDER", "anthropic");
        let cfg = VisionConfig::from_env().expect("ok");
        assert_eq!(cfg.provider, VisionProvider::Anthropic);
    }

    #[test]
    fn explicit_provider_openai_wins_over_anthropic_base_url() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        std::env::set_var("NUPHUS_MCP_VISION_BASE_URL", "https://api.anthropic.com/v1");
        std::env::set_var("NUPHUS_MCP_VISION_PROVIDER", "openai");
        let cfg = VisionConfig::from_env().expect("ok");
        assert_eq!(cfg.provider, VisionProvider::OpenAI);
    }

    #[test]
    fn invalid_provider_value_errors() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        std::env::set_var("NUPHUS_MCP_VISION_PROVIDER", "banana");
        let err = VisionConfig::from_env().expect_err("invalid provider must fail");
        assert!(
            err.contains("NUPHUS_MCP_VISION_PROVIDER"),
            "error must name the env var: {err}"
        );
        assert!(
            err.contains("auto|openai|anthropic"),
            "error must list options: {err}"
        );
    }

    #[test]
    fn max_tokens_env_override() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        std::env::set_var("NUPHUS_MCP_VISION_MAX_TOKENS", "1024");
        let cfg = VisionConfig::from_env().expect("ok");
        assert_eq!(
            cfg.max_tokens, 1024,
            "explicit 1024 (Zhipu GLM-4V-Flash cap) accepted"
        );
        std::env::set_var("NUPHUS_MCP_VISION_MAX_TOKENS", "4096");
        let cfg = VisionConfig::from_env().expect("ok");
        assert_eq!(
            cfg.max_tokens, 4096,
            "higher values allowed for providers that support them"
        );
    }

    #[test]
    fn max_tokens_invalid_errors() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();
        key_model();
        std::env::set_var("NUPHUS_MCP_VISION_MAX_TOKENS", "abc");
        let err = VisionConfig::from_env().expect_err("non-integer must fail");
        assert!(
            err.contains("NUPHUS_MCP_VISION_MAX_TOKENS"),
            "error must name the env var: {err}"
        );
        std::env::set_var("NUPHUS_MCP_VISION_MAX_TOKENS", "0");
        let err = VisionConfig::from_env().expect_err("0 must fail");
        assert!(
            err.contains("1..=32768"),
            "error must state the range: {err}"
        );
        std::env::set_var("NUPHUS_MCP_VISION_MAX_TOKENS", "999999");
        let err = VisionConfig::from_env().expect_err("overflow must fail");
        assert!(
            err.contains("1..=32768"),
            "error must state the range: {err}"
        );
    }

    // ---- Protocol request/response shapes ----

    #[test]
    fn openai_request_uses_image_url_block() {
        let (url, body) = openai_request(
            "gpt-4o-mini",
            "https://api.openai.com/v1",
            "image/png",
            "QUJD",
            "hi",
            1024,
        );
        assert!(url.ends_with("/chat/completions"), "url: {url}");
        assert_eq!(body["model"], "gpt-4o-mini");
        assert_eq!(body["max_tokens"], 1024);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
        let data_url = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(
            data_url.starts_with("data:image/png;base64,QUJD"),
            "data url: {data_url}"
        );
        assert!(!body.to_string().contains("\"source\""));
    }

    #[test]
    fn anthropic_request_uses_native_image_block() {
        let (url, body) = anthropic_request(
            "claude-sonnet-4-5",
            "https://api.anthropic.com/v1",
            "image/png",
            "QUJD",
            "what is this",
            4096,
        );
        assert!(url.ends_with("/messages"), "url: {url}");
        assert_eq!(body["model"], "claude-sonnet-4-5");
        assert_eq!(body["max_tokens"], 4096);
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "QUJD");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "what is this");
        // Native protocol must NOT use the OpenAI image_url shape.
        let raw = body.to_string();
        assert!(
            !raw.contains("image_url"),
            "must not use image_url block: {raw}"
        );
    }

    #[test]
    fn anthropic_headers_use_x_api_key() {
        let headers = provider_headers(VisionProvider::Anthropic, "sk-ant-abc");
        let map: HashMap<&str, &str> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(map.get("x-api-key"), Some(&"sk-ant-abc"));
        assert_eq!(map.get("anthropic-version"), Some(&"2023-06-01"));
        assert!(
            map.get("authorization").is_none(),
            "anthropic must not use bearer"
        );
    }

    #[test]
    fn openai_headers_use_bearer() {
        let headers = provider_headers(VisionProvider::OpenAI, "sk-abc");
        let map: HashMap<&str, &str> = headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        assert_eq!(map.get("authorization"), Some(&"Bearer sk-abc"));
        assert!(map.get("x-api-key").is_none());
    }

    #[test]
    fn anthropic_response_joins_text_blocks() {
        let resp = json!({
            "content": [
                { "type": "text", "text": "The button says " },
                { "type": "text", "text": "Save." }
            ]
        });
        assert_eq!(
            parse_anthropic_response(&resp).unwrap(),
            "The button says Save."
        );
    }

    #[test]
    fn anthropic_response_empty_content_errors() {
        let resp = json!({ "content": [{ "type": "text", "text": "   " }] });
        assert!(parse_anthropic_response(&resp).is_err());
    }

    #[test]
    fn openai_response_extracts_content() {
        let resp = json!({
            "choices": [ { "message": { "content": "a login form" } } ]
        });
        assert_eq!(parse_openai_response(&resp).unwrap(), "a login form");
    }

    // ---- End-to-end (local mock endpoint, real HTTP path) ----

    /// 1x1 black PNG for the e2e test.
    fn make_test_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255]));
        let mut buf = std::io::Cursor::new(Vec::new());
        img.write_to(&mut buf, image::ImageFormat::Png)
            .expect("encode test png");
        buf.into_inner()
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    /// Parse `Content-Length` out of a raw HTTP header block.
    fn parse_content_length(head: &str) -> usize {
        head.lines()
            .find_map(|l| {
                let (k, v) = l.split_once(':')?;
                if k.trim().eq_ignore_ascii_case("content-length") {
                    v.trim().parse().ok()
                } else {
                    None
                }
            })
            .unwrap_or(0)
    }

    /// End-to-end: point the Anthropic backend at a local mock server; assert the
    /// wire request uses the native protocol and the response is parsed correctly.
    /// Sync `#[test]` + `block_on` so the env lock is held across the await (an async
    /// test would need the future Send, but `MutexGuard` is not Send).
    #[test]
    fn anthropic_provider_end_to_end_local_server() {
        let _lock = env_lock();
        let _g = EnvGuard::clear();

        let png = make_test_png();
        let img_path =
            std::env::temp_dir().join(format!("nuphus_vision_e2e_{}.png", std::process::id()));
        std::fs::write(&img_path, &png).expect("write test png");
        let img_str = img_path.to_str().unwrap().to_string();
        let _cleanup = TestFile(&img_path); // removes on drop

        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            let addr = listener.local_addr().expect("local addr");
            let port = addr.port();

            let server = tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let (mut sock, _) = listener.accept().await.expect("accept");
                let mut buf = Vec::new();
                let mut chunk = [0u8; 2048];
                loop {
                    let n = sock.read(&mut chunk).await.expect("read");
                    if n == 0 {
                        break;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    // Request complete once headers and full body are in.
                    if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                        let body_len = parse_content_length(&head);
                        if buf.len() >= pos + 4 + body_len {
                            break;
                        }
                    }
                }
                let raw = String::from_utf8_lossy(&buf).to_string();
                let body = r#"{"content":[{"type":"text","text":"The login button is on the top right."}]}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                sock.write_all(resp.as_bytes()).await.expect("write resp");
                sock.shutdown().await.expect("shutdown");
                raw
            });

            let base_url = format!("http://127.0.0.1:{port}/v1");
            std::env::set_var("NUPHUS_MCP_VISION_API_KEY", "sk-ant-test");
            std::env::set_var("NUPHUS_MCP_VISION_MODEL", "claude-sonnet-4-5");
            std::env::set_var("NUPHUS_MCP_VISION_BASE_URL", &base_url);
            std::env::set_var("NUPHUS_MCP_VISION_PROVIDER", "anthropic");

            let text = vision_image(&img_str, None)
                .await
                .expect("vision call succeeds");
            assert_eq!(text, "The login button is on the top right.");

            let raw = server.await.expect("server finished");
            let lower = raw.to_ascii_lowercase();
            assert!(lower.contains("x-api-key: sk-ant-test"), "must send x-api-key: {raw}");
            assert!(
                lower.contains("anthropic-version: 2023-06-01"),
                "must send anthropic-version: {raw}"
            );
            assert!(raw.contains("/messages"), "must hit /messages: {raw}");
            assert!(raw.contains("\"type\":\"image\""), "must use native image block: {raw}");
            assert!(
                raw.contains("\"media_type\":\"image/png\""),
                "must carry media_type: {raw}"
            );
            assert!(!raw.contains("image_url"), "must NOT use image_url block: {raw}");
        });
    }

    /// Removes a temp file on drop (test cleanup).
    struct TestFile<'a>(&'a std::path::Path);
    impl Drop for TestFile<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
}
