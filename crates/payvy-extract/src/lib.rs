//! Reads a bill from its document with a model.
//!
//! Two backends cover nearly every model a self-hoster would pick:
//!
//! - **Claude**, through the Messages API, which reads PDFs and images
//!   directly and constrains its answer to the schema.
//! - **Any OpenAI-compatible chat completions API**: OpenAI, Gemini
//!   (`generativelanguage.googleapis.com/v1beta/openai`), Grok (`api.x.ai/v1`),
//!   OpenRouter, or a model on your own machine through Ollama or vLLM, so
//!   invoices never leave it.
//!
//! Both ask for the same schema (`payvy_domain::extraction::schema`) and
//! return the same [`payvy_domain::extraction::Read`]. Checking the answer is
//! the domain's job, not the model's.

use std::time::Duration;

use base64::Engine;
use payvy_domain::ai_usage::TokenUsage;
use payvy_domain::documents::Kind;
use payvy_domain::extraction::{self, Read};
use serde_json::{Value, json};

/// The Claude API version these requests are written for.
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// The default Claude model.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-sonnet-5-5";
/// Timeout for a single model HTTP request, also used to treat stale pending
/// usage attempts as unknown in `usage.summary`.
pub const REQUEST_TIMEOUT_SECS: i64 = 300;
/// Claude's limit is 32 MB per request; base64 adds a third.
const MAX_ENCODED: usize = 30 * 1024 * 1024;
const PROMPT: &str = "Read this document.";

/// What the model is asked, with the pages to read when the document is
/// only some of the file's pages (split from it, or with pages hidden). The
/// file sent is always the whole original; page numbers are the file's.
pub fn prompt(pages: Option<&[u32]>) -> String {
    match pages {
        None => PROMPT.to_owned(),
        Some(pages) => {
            let list: Vec<String> = pages.iter().map(u32::to_string).collect();
            let word = if pages.len() == 1 { "page" } else { "pages" };
            format!(
                "{PROMPT} Read only {word} {} of this file, in that order; the other pages belong \
                 to other documents. Give page numbers as pages of the whole file.",
                list.join(", ")
            )
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    Anthropic,
    OpenAiCompatible,
}

impl Provider {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Some(Provider::Anthropic),
            "openai" | "openai-compatible" | "openai_compatible" => {
                Some(Provider::OpenAiCompatible)
            }
            _ => None,
        }
    }

    pub fn default_base_url(self) -> &'static str {
        match self {
            Provider::Anthropic => "https://api.anthropic.com",
            Provider::OpenAiCompatible => "https://api.openai.com/v1",
        }
    }
}

#[derive(Clone)]
pub struct Config {
    pub provider: Provider,
    /// Anthropic: https://api.anthropic.com. OpenAI-compatible: the URL that
    /// `/chat/completions` hangs off, e.g. https://api.openai.com/v1 or
    /// http://127.0.0.1:11434/v1.
    pub base_url: String,
    /// Optional for a local server that doesn't check one.
    pub api_key: Option<String>,
    pub model: String,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("provider", &self.provider)
            .field("base_url", &self.base_url)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ExtractError {
    /// This document can't be sent to this provider (a TIFF, or too large).
    #[error("{0}")]
    Unsupported(String),
    /// The model declined to read it.
    #[error("the model declined to read this document: {0}")]
    Refused(String),
    /// The provider failed or answered with something that isn't the schema.
    #[error("{0}")]
    Failed(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptOutcome {
    Succeeded,
    Refused,
    Truncated,
    InvalidOutput,
    ProviderError,
    Timeout,
    Unsupported,
}

impl AttemptOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Refused => "refused",
            Self::Truncated => "truncated",
            Self::InvalidOutput => "invalid_output",
            Self::ProviderError => "provider_error",
            Self::Timeout => "timeout",
            Self::Unsupported => "unsupported",
        }
    }

    /// Short fixed error label safe to store in `ai_usage_attempts.error`
    /// without echoing raw provider bodies or invoice content.
    pub fn fixed_error_message(self) -> Option<&'static str> {
        match self {
            Self::Succeeded => None,
            Self::Refused => Some("model refused"),
            Self::Truncated => Some("response truncated"),
            Self::InvalidOutput => Some("invalid model output"),
            Self::ProviderError => Some("provider error"),
            Self::Timeout => Some("request timed out"),
            Self::Unsupported => Some("unsupported document"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AttemptTelemetry {
    pub actual_model: Option<String>,
    pub provider_request_id: Option<String>,
    pub usage: TokenUsage,
}

#[derive(Debug)]
pub struct ExtractAttempt {
    pub result: Result<Output, ExtractError>,
    pub outcome: AttemptOutcome,
    pub telemetry: AttemptTelemetry,
}

#[derive(Debug, Clone)]
pub struct Output {
    pub read: Read,
    /// The answer exactly as the model gave it, kept for the record.
    pub raw: Value,
    /// The model that answered, as the provider reports it (a fallback may
    /// differ from the one asked for).
    pub model: String,
}

pub struct Extractor {
    config: Config,
    http: reqwest::Client,
}

impl Extractor {
    pub fn new(config: Config) -> anyhow::Result<Self> {
        Self::with_timeout(config, Duration::from_secs(REQUEST_TIMEOUT_SECS as u64))
    }

    pub fn with_timeout(config: Config, timeout: Duration) -> anyhow::Result<Self> {
        anyhow::ensure!(!config.model.trim().is_empty(), "no extraction model set");
        let http = reqwest::Client::builder().timeout(timeout).build()?;
        Ok(Self { config, http })
    }

    /// "anthropic" or "openai-compatible (api.x.ai)": what's recorded with
    /// each extraction.
    pub fn provider_label(&self) -> String {
        match self.config.provider {
            Provider::Anthropic => "anthropic".into(),
            Provider::OpenAiCompatible => {
                let host = reqwest::Url::parse(&self.config.base_url)
                    .ok()
                    .and_then(|u| u.host_str().map(str::to_owned))
                    .unwrap_or_default();
                format!("openai-compatible ({host})")
            }
        }
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    pub async fn read(&self, bytes: &[u8], kind: Kind) -> Result<Output, ExtractError> {
        self.read_pages(bytes, kind, None).await
    }

    /// Reads only `pages` of the file (its own page numbers, in this order)
    /// when given; the whole file otherwise.
    pub async fn read_pages(
        &self,
        bytes: &[u8],
        kind: Kind,
        pages: Option<&[u32]>,
    ) -> Result<Output, ExtractError> {
        self.read_pages_tracked(bytes, kind, pages).await.result
    }

    pub async fn read_pages_tracked(
        &self,
        bytes: &[u8],
        kind: Kind,
        pages: Option<&[u32]>,
    ) -> ExtractAttempt {
        self.read_mode(bytes, kind, pages, false).await
    }

    pub async fn read_pages_v2(
        &self,
        bytes: &[u8],
        kind: Kind,
        pages: &[u32],
    ) -> Result<Output, ExtractError> {
        self.read_pages_v2_tracked(bytes, kind, pages).await.result
    }

    pub async fn read_pages_v2_tracked(
        &self,
        bytes: &[u8],
        kind: Kind,
        pages: &[u32],
    ) -> ExtractAttempt {
        if pages.is_empty() || pages.len() > 24 {
            return ExtractAttempt {
                result: Err(ExtractError::Unsupported(
                    "Extraction v2 supports 1–24 selected pages. Split only at verified invoice boundaries; a single longer invoice needs manual review until page batching is available.".into(),
                )),
                outcome: AttemptOutcome::Unsupported,
                telemetry: AttemptTelemetry::default(),
            };
        }
        self.read_mode(bytes, kind, Some(pages), true).await
    }

    async fn read_mode(
        &self,
        bytes: &[u8],
        kind: Kind,
        pages: Option<&[u32]>,
        v2: bool,
    ) -> ExtractAttempt {
        let mut body = match request_pages(&self.config, bytes, kind, pages) {
            Ok(body) => body,
            Err(err) => {
                return ExtractAttempt {
                    result: Err(err),
                    outcome: AttemptOutcome::Unsupported,
                    telemetry: AttemptTelemetry::default(),
                };
            }
        };
        if v2 {
            match self.config.provider {
                Provider::Anthropic => {
                    body["system"] = json!(payvy_domain::invoice_evidence::INSTRUCTIONS);
                    body["output_config"]["format"]["schema"] = extraction::schema_v2();
                }
                Provider::OpenAiCompatible => {
                    body["messages"][0]["content"] =
                        json!(payvy_domain::invoice_evidence::INSTRUCTIONS);
                    body["response_format"]["json_schema"]["schema"] = extraction::schema_v2();
                }
            }
        }
        let base = self.config.base_url.trim_end_matches('/');
        let req = match self.config.provider {
            Provider::Anthropic => {
                let mut r = self
                    .http
                    .post(format!("{base}/v1/messages"))
                    .header("anthropic-version", ANTHROPIC_VERSION);
                if let Some(k) = &self.config.api_key {
                    r = r.header("x-api-key", k);
                }
                if claude_fallback(&self.config.model) {
                    r = r.header("anthropic-beta", "server-side-fallback-2026-07-01");
                }
                r
            }
            Provider::OpenAiCompatible => {
                let r = self.http.post(format!("{base}/chat/completions"));
                match &self.config.api_key {
                    Some(k) => r.bearer_auth(k),
                    None => r,
                }
            }
        };
        let mut res = match req.json(&body).send().await {
            Ok(res) => res,
            Err(e) => {
                let outcome = if e.is_timeout() {
                    AttemptOutcome::Timeout
                } else {
                    AttemptOutcome::ProviderError
                };
                return ExtractAttempt {
                    result: Err(ExtractError::Failed(format!(
                        "couldn't reach the model: {e}"
                    ))),
                    outcome,
                    telemetry: AttemptTelemetry::default(),
                };
            }
        };
        let status = res.status();
        let header_request_id = res
            .headers()
            .get("request-id")
            .or_else(|| res.headers().get("x-request-id"))
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let mut bytes = Vec::new();
        loop {
            match res.chunk().await {
                Ok(Some(chunk)) => {
                    if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                        return ExtractAttempt {
                            result: Err(ExtractError::Failed(
                                "The model response exceeded the 4 MiB limit".into(),
                            )),
                            outcome: AttemptOutcome::ProviderError,
                            telemetry: AttemptTelemetry {
                                provider_request_id: header_request_id,
                                ..Default::default()
                            },
                        };
                    }
                    bytes.extend_from_slice(&chunk);
                }
                Ok(None) => break,
                Err(e) => {
                    let outcome = if e.is_timeout() {
                        AttemptOutcome::Timeout
                    } else {
                        AttemptOutcome::ProviderError
                    };
                    return ExtractAttempt {
                        result: Err(ExtractError::Failed(
                            "The model response was cut off".into(),
                        )),
                        outcome,
                        telemetry: AttemptTelemetry {
                            provider_request_id: header_request_id,
                            ..Default::default()
                        },
                    };
                }
            }
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                return ExtractAttempt {
                    result: Err(ExtractError::Failed(
                        "The model response was not UTF-8".into(),
                    )),
                    outcome: AttemptOutcome::InvalidOutput,
                    telemetry: AttemptTelemetry {
                        provider_request_id: header_request_id,
                        ..Default::default()
                    },
                };
            }
        };
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let mut telemetry = telemetry_from_value(self.config.provider, &value);
        if telemetry.provider_request_id.is_none() {
            telemetry.provider_request_id = header_request_id;
        }
        if !status.is_success() {
            return ExtractAttempt {
                result: Err(ExtractError::Failed(format!(
                    "the model's API answered {status}"
                ))),
                outcome: AttemptOutcome::ProviderError,
                telemetry,
            };
        }
        let parsed = match self.config.provider {
            Provider::Anthropic => parse_anthropic(&value, &self.config.model),
            Provider::OpenAiCompatible => parse_openai(&value, &self.config.model),
        };
        match parsed {
            Err((err, outcome)) => ExtractAttempt {
                result: Err(err),
                outcome,
                telemetry,
            },
            Ok(out) => {
                if v2
                    && (out.read.coverage.is_none()
                        || out.read.lines.len() > 1000
                        || out.read.lines.iter().any(|l| {
                            l.details.as_ref().is_none_or(|d| {
                                !d.bounded()
                                    || payvy_domain::invoice_evidence::FIELDS
                                        .iter()
                                        .any(|f| !d.states.contains_key(*f))
                            })
                        }))
                {
                    return ExtractAttempt {
                        result: Err(ExtractError::Failed("The model omitted v2 coverage/field states or exceeded row/evidence limits; review and read again".into())),
                        outcome: AttemptOutcome::InvalidOutput,
                        telemetry,
                    };
                }
                ExtractAttempt {
                    result: Ok(out),
                    outcome: AttemptOutcome::Succeeded,
                    telemetry,
                }
            }
        }
    }
}

/// Claude models that take the server-side fallback for a declined
/// request; the rest stop with a refusal.
fn claude_fallback(model: &str) -> bool {
    model.starts_with("claude-opus-5")
        || model.starts_with("claude-fable-5")
        || model == "claude-sonnet-5-5"
}

/// The request body for this provider.
pub fn request(config: &Config, bytes: &[u8], kind: Kind) -> Result<Value, ExtractError> {
    request_pages(config, bytes, kind, None)
}

/// The request body, asking for only `pages` of the file when given.
pub fn request_pages(
    config: &Config,
    bytes: &[u8],
    kind: Kind,
    pages: Option<&[u32]>,
) -> Result<Value, ExtractError> {
    let prompt = prompt(pages);
    if kind == Kind::Tiff {
        return Err(ExtractError::Unsupported(
            "TIFF scans can't be read automatically yet; convert it to PDF".into(),
        ));
    }
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    if data.len() > MAX_ENCODED {
        return Err(ExtractError::Unsupported(
            "this file is too large to read automatically".into(),
        ));
    }
    let schema = extraction::schema();
    Ok(match config.provider {
        Provider::Anthropic => {
            let block = if kind == Kind::Pdf {
                json!({ "type": "document", "source": { "type": "base64", "media_type": "application/pdf", "data": data } })
            } else {
                json!({ "type": "image", "source": { "type": "base64", "media_type": kind.content_type(), "data": data } })
            };
            let mut body = json!({
                "model": config.model,
                "max_tokens": 16000,
                "system": extraction::INSTRUCTIONS,
                "messages": [{ "role": "user", "content": [block, { "type": "text", "text": prompt }] }],
                "output_config": { "format": { "type": "json_schema", "schema": schema } },
            });
            if claude_fallback(&config.model) {
                body["fallbacks"] = json!("default");
            }
            body
        }
        Provider::OpenAiCompatible => {
            let url = format!("data:{};base64,{data}", kind.content_type());
            let part = if kind == Kind::Pdf {
                json!({ "type": "file", "file": { "filename": "document.pdf", "file_data": url } })
            } else {
                json!({ "type": "image_url", "image_url": { "url": url } })
            };
            json!({
                "model": config.model,
                "messages": [
                    { "role": "system", "content": extraction::INSTRUCTIONS },
                    { "role": "user", "content": [part, { "type": "text", "text": prompt }] },
                ],
                "response_format": {
                    "type": "json_schema",
                    "json_schema": { "name": "document", "strict": true, "schema": schema },
                },
            })
        }
    })
}

fn telemetry_from_value(provider: Provider, v: &Value) -> AttemptTelemetry {
    let actual_model = v["model"].as_str().map(str::to_owned);
    let provider_request_id = v["id"].as_str().map(str::to_owned);
    let u = &v["usage"];
    let usage = if u.is_object() {
        match provider {
            Provider::Anthropic => TokenUsage {
                // Anthropic's input_tokens excludes cache_read and cache_creation.
                input_tokens: u["input_tokens"].as_i64(),
                output_tokens: u["output_tokens"].as_i64(),
                cache_read_tokens: u["cache_read_input_tokens"].as_i64(),
                cache_write_tokens: u["cache_creation_input_tokens"].as_i64(),
            },
            Provider::OpenAiCompatible => {
                let cache_read = u["prompt_tokens_details"]["cached_tokens"].as_i64();
                let cache_write = u["prompt_tokens_details"]["cache_write_tokens"].as_i64();
                // OpenAI's prompt_tokens includes cached_tokens; subtract so cache
                // reads aren't double-counted.
                let input = u["prompt_tokens"]
                    .as_i64()
                    .map(|p| p.saturating_sub(cache_read.unwrap_or(0)));
                TokenUsage {
                    input_tokens: input,
                    output_tokens: u["completion_tokens"].as_i64(),
                    cache_read_tokens: cache_read,
                    cache_write_tokens: cache_write,
                }
            }
        }
    } else {
        TokenUsage::default()
    };
    AttemptTelemetry {
        actual_model,
        provider_request_id,
        usage,
    }
}

fn parse_anthropic(v: &Value, asked: &str) -> Result<Output, (ExtractError, AttemptOutcome)> {
    match v["stop_reason"].as_str() {
        Some("refusal") => {
            let why = v["stop_details"]["explanation"]
                .as_str()
                .or_else(|| v["stop_details"]["category"].as_str())
                .unwrap_or("no reason given");
            return Err((ExtractError::Refused(why.into()), AttemptOutcome::Refused));
        }
        Some("max_tokens") => {
            return Err((
                ExtractError::Failed("the model ran out of room before finishing".into()),
                AttemptOutcome::Truncated,
            ));
        }
        _ => {}
    }
    let text = v["content"]
        .as_array()
        .and_then(|blocks| {
            blocks
                .iter()
                .find(|b| b["type"] == "text")
                .and_then(|b| b["text"].as_str())
        })
        .ok_or_else(|| {
            (
                ExtractError::Failed("the model's answer had no text".into()),
                AttemptOutcome::InvalidOutput,
            )
        })?;
    output(text, v["model"].as_str().unwrap_or(asked))
        .map_err(|e| (e, AttemptOutcome::InvalidOutput))
}

fn parse_openai(v: &Value, asked: &str) -> Result<Output, (ExtractError, AttemptOutcome)> {
    let choice = &v["choices"][0];
    if let Some(r) = choice["message"]["refusal"].as_str() {
        return Err((ExtractError::Refused(r.into()), AttemptOutcome::Refused));
    }
    if choice["finish_reason"] == "length" {
        return Err((
            ExtractError::Failed("the model ran out of room before finishing".into()),
            AttemptOutcome::Truncated,
        ));
    }
    let text = choice["message"]["content"].as_str().ok_or_else(|| {
        (
            ExtractError::Failed("the model's answer had no text".into()),
            AttemptOutcome::InvalidOutput,
        )
    })?;
    output(text, v["model"].as_str().unwrap_or(asked))
        .map_err(|e| (e, AttemptOutcome::InvalidOutput))
}

/// Parses the answer. Providers without strict schemas sometimes wrap the
/// JSON in a code fence or a sentence, so this takes the outermost object.
fn output(text: &str, model: &str) -> Result<Output, ExtractError> {
    let json = match (text.find('{'), text.rfind('}')) {
        (Some(a), Some(b)) if a < b => &text[a..=b],
        _ => text,
    };
    let raw: Value = serde_json::from_str(json)
        .map_err(|e| ExtractError::Failed(format!("the model's answer isn't JSON: {e}")))?;
    let read: Read = serde_json::from_value(raw.clone()).map_err(|e| {
        ExtractError::Failed(format!("the model's answer doesn't fit the schema: {e}"))
    })?;
    Ok(Output {
        read,
        raw,
        model: model.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(provider: Provider, model: &str) -> Config {
        Config {
            provider,
            base_url: provider.default_base_url().into(),
            api_key: Some("k".into()),
            model: model.into(),
        }
    }

    const ANSWER: &str = r#"{"kind":"invoice","vendor_name":"King Environmental","vendor_email":null,"vendor_address":null,"account_number":null,"invoice_number":"189727","invoice_date":"2026-11-01","due_date":null,"terms":null,"currency":"USD","po_number":null,"subtotal":null,"tax":null,"total":"459.00","lines":[],"notes":null}"#;

    #[test]
    fn claude_gets_a_document_block_and_the_schema() {
        let body = request(
            &config(Provider::Anthropic, DEFAULT_CLAUDE_MODEL),
            b"%PDF-1.7",
            Kind::Pdf,
        )
        .unwrap();
        let content = &body["messages"][0]["content"];
        assert_eq!(content[0]["type"], "document");
        assert_eq!(content[0]["source"]["data"], "JVBERi0xLjc=");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["fallbacks"], "default");
        assert!(body.get("temperature").is_none());

        let body = request(
            &config(Provider::Anthropic, "claude-haiku-5-5"),
            b"\x89PNG",
            Kind::Png,
        )
        .unwrap();
        assert_eq!(body["messages"][0]["content"][0]["type"], "image");
        assert!(body.get("fallbacks").is_none());
    }

    #[test]
    fn openai_compatible_gets_a_data_url_and_a_strict_schema() {
        let body = request(
            &config(Provider::OpenAiCompatible, "gpt-5"),
            b"%PDF-1.7",
            Kind::Pdf,
        )
        .unwrap();
        let part = &body["messages"][1]["content"][0];
        assert_eq!(part["type"], "file");
        assert_eq!(
            part["file"]["file_data"],
            "data:application/pdf;base64,JVBERi0xLjc="
        );
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        let body = request(
            &config(Provider::OpenAiCompatible, "llama"),
            b"\xFF\xD8\xFF",
            Kind::Jpeg,
        )
        .unwrap();
        assert_eq!(body["messages"][1]["content"][0]["type"], "image_url");
    }

    #[test]
    fn a_document_of_some_pages_asks_for_only_those() {
        let body = request_pages(
            &config(Provider::Anthropic, DEFAULT_CLAUDE_MODEL),
            b"%PDF-1.7",
            Kind::Pdf,
            Some(&[4, 5]),
        )
        .unwrap();
        let text = body["messages"][0]["content"][1]["text"].as_str().unwrap();
        assert!(text.starts_with("Read this document."), "{text}");
        assert!(text.contains("Read only pages 4, 5 of this file"), "{text}");
        let body = request_pages(
            &config(Provider::OpenAiCompatible, "gpt-5"),
            b"%PDF-1.7",
            Kind::Pdf,
            Some(&[2]),
        )
        .unwrap();
        assert!(
            body["messages"][1]["content"][1]["text"]
                .as_str()
                .unwrap()
                .contains("Read only page 2 of this file")
        );
        // The whole file: the prompt as it always was.
        let body = request(&config(Provider::Anthropic, "m"), b"%PDF-1.7", Kind::Pdf).unwrap();
        assert_eq!(
            body["messages"][0]["content"][1]["text"],
            "Read this document."
        );
    }

    #[test]
    fn tiffs_are_refused_before_sending() {
        assert!(matches!(
            request(&config(Provider::Anthropic, "m"), b"II*\0", Kind::Tiff),
            Err(ExtractError::Unsupported(_))
        ));
    }

    #[test]
    fn answers_parse_from_either_provider() {
        let claude = json!({ "model": "claude-opus-5-5", "stop_reason": "end_turn",
            "content": [{ "type": "thinking", "thinking": "" }, { "type": "text", "text": ANSWER }] });
        let out = parse_anthropic(&claude, "x").unwrap();
        assert_eq!(out.model, "claude-opus-5-5");
        assert_eq!(out.read.total.as_deref(), Some("459.00"));

        let fenced = format!("Here you go:\n```json\n{ANSWER}\n```");
        let openai = json!({ "model": "grok-4", "choices": [{ "finish_reason": "stop", "message": { "content": fenced } }] });
        assert_eq!(
            parse_openai(&openai, "x")
                .unwrap()
                .read
                .invoice_number
                .as_deref(),
            Some("189727")
        );
    }

    #[test]
    fn refusals_and_cut_offs_are_errors() {
        let refused = json!({ "stop_reason": "refusal", "stop_details": { "category": "cyber" }, "content": [] });
        assert_eq!(
            parse_anthropic(&refused, "x").unwrap_err(),
            (
                ExtractError::Refused("cyber".into()),
                AttemptOutcome::Refused
            )
        );
        let cut =
            json!({ "choices": [{ "finish_reason": "length", "message": { "content": "{" } }] });
        assert!(matches!(
            parse_openai(&cut, "x"),
            Err((ExtractError::Failed(_), AttemptOutcome::Truncated))
        ));
        let wrong = json!({ "choices": [{ "message": { "content": "{\"kind\":\"banana\"}" } }] });
        assert!(matches!(
            parse_openai(&wrong, "x"),
            Err((ExtractError::Failed(_), AttemptOutcome::InvalidOutput))
        ));
    }

    #[test]
    fn usage_normalization_avoids_double_counting_cache_tokens() {
        let claude = json!({
            "id": "msg_123",
            "model": "claude-sonnet-5-5",
            "usage": {
                "input_tokens": 200,
                "output_tokens": 50,
                "cache_read_input_tokens": 1000,
                "cache_creation_input_tokens": 300
            }
        });
        let t = telemetry_from_value(Provider::Anthropic, &claude);
        assert_eq!(t.provider_request_id.as_deref(), Some("msg_123"));
        assert_eq!(t.actual_model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            t.usage,
            TokenUsage {
                input_tokens: Some(200),
                output_tokens: Some(50),
                cache_read_tokens: Some(1000),
                cache_write_tokens: Some(300),
            }
        );

        let openai = json!({
            "id": "chatcmpl-456",
            "model": "gpt-5",
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 80,
                "prompt_tokens_details": { "cached_tokens": 1000 }
            }
        });
        let t = telemetry_from_value(Provider::OpenAiCompatible, &openai);
        assert_eq!(t.provider_request_id.as_deref(), Some("chatcmpl-456"));
        assert_eq!(t.actual_model.as_deref(), Some("gpt-5"));
        assert_eq!(
            t.usage,
            TokenUsage {
                input_tokens: Some(200),
                output_tokens: Some(80),
                cache_read_tokens: Some(1000),
                cache_write_tokens: None,
            }
        );
    }
}
