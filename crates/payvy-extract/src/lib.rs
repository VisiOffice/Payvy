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
use payvy_domain::documents::Kind;
use payvy_domain::extraction::{self, Read};
use serde_json::{Value, json};

/// The Claude API version these requests are written for.
const ANTHROPIC_VERSION: &str = "2026-10-21";
/// The default Claude model.
pub const DEFAULT_CLAUDE_MODEL: &str = "claude-opus-5-5";
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
        anyhow::ensure!(!config.model.trim().is_empty(), "no extraction model set");
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(300))
            .build()?;
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
        self.read_mode(bytes, kind, pages, false).await
    }

    pub async fn read_pages_v2(
        &self,
        bytes: &[u8],
        kind: Kind,
        pages: &[u32],
    ) -> Result<Output, ExtractError> {
        if pages.is_empty() || pages.len() > 24 {
            return Err(ExtractError::Unsupported(
                "Extraction v2 supports 1–24 selected pages. Split only at verified invoice boundaries; a single longer invoice needs manual review until page batching is available.".into(),
            ));
        }
        self.read_mode(bytes, kind, Some(pages), true).await
    }

    async fn read_mode(
        &self,
        bytes: &[u8],
        kind: Kind,
        pages: Option<&[u32]>,
        v2: bool,
    ) -> Result<Output, ExtractError> {
        let mut body = request_pages(&self.config, bytes, kind, pages)?;
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
        let mut res = req
            .json(&body)
            .send()
            .await
            .map_err(|e| ExtractError::Failed(format!("couldn't reach the model: {e}")))?;
        let status = res.status();
        let mut bytes = Vec::new();
        while let Some(chunk) = res
            .chunk()
            .await
            .map_err(|_| ExtractError::Failed("The model response was cut off".into()))?
        {
            if bytes.len() + chunk.len() > 4 * 1024 * 1024 {
                return Err(ExtractError::Failed(
                    "The model response exceeded the 4 MiB limit".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let text = String::from_utf8(bytes)
            .map_err(|_| ExtractError::Failed("The model response was not UTF-8".into()))?;
        let value: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if !status.is_success() {
            return Err(ExtractError::Failed(format!(
                "the model's API answered {status}"
            )));
        }
        let out = match self.config.provider {
            Provider::Anthropic => parse_anthropic(&value, &self.config.model),
            Provider::OpenAiCompatible => parse_openai(&value, &self.config.model),
        }?;
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
            return Err(ExtractError::Failed("The model omitted v2 coverage/field states or exceeded row/evidence limits; review and read again".into()));
        }
        Ok(out)
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

fn parse_anthropic(v: &Value, asked: &str) -> Result<Output, ExtractError> {
    match v["stop_reason"].as_str() {
        Some("refusal") => {
            let why = v["stop_details"]["explanation"]
                .as_str()
                .or_else(|| v["stop_details"]["category"].as_str())
                .unwrap_or("no reason given");
            return Err(ExtractError::Refused(why.into()));
        }
        Some("max_tokens") => {
            return Err(ExtractError::Failed(
                "the model ran out of room before finishing".into(),
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
        .ok_or_else(|| ExtractError::Failed("the model's answer had no text".into()))?;
    output(text, v["model"].as_str().unwrap_or(asked))
}

fn parse_openai(v: &Value, asked: &str) -> Result<Output, ExtractError> {
    let choice = &v["choices"][0];
    if let Some(r) = choice["message"]["refusal"].as_str() {
        return Err(ExtractError::Refused(r.into()));
    }
    if choice["finish_reason"] == "length" {
        return Err(ExtractError::Failed(
            "the model ran out of room before finishing".into(),
        ));
    }
    let text = choice["message"]["content"]
        .as_str()
        .ok_or_else(|| ExtractError::Failed("the model's answer had no text".into()))?;
    output(text, v["model"].as_str().unwrap_or(asked))
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
            ExtractError::Refused("cyber".into())
        );
        let cut =
            json!({ "choices": [{ "finish_reason": "length", "message": { "content": "{" } }] });
        assert!(matches!(
            parse_openai(&cut, "x"),
            Err(ExtractError::Failed(_))
        ));
        let wrong = json!({ "choices": [{ "message": { "content": "{\"kind\":\"banana\"}" } }] });
        assert!(matches!(
            parse_openai(&wrong, "x"),
            Err(ExtractError::Failed(_))
        ));
    }
}
