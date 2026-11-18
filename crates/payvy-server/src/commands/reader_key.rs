//! A company's own model key for Payvy's reader (Settings > Agents >
//! Reading). With one, `document.extract` and the background reader read the
//! company's invoices with its key, billed to the company by its provider,
//! whatever its plan's built-in reader. The key is checked with the provider
//! (a free model lookup), sealed with PAYVY_SECRETS_KEY, and never returned,
//! logged or put in an event: only its last four characters are kept in the
//! clear. Each provider's address is fixed here, so a key only ever goes to
//! that provider: Anthropic, OpenAI, Google (Gemini), xAI (Grok) or Meta
//! (Muse). All but Anthropic are reached through their OpenAI-compatible API.

use std::ops::Deref;

use payvy_extract::{Config, Extractor, Provider};
use payvy_store::{self as store, ReaderKey};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CommandError, Cx};

/// What the sealed key is bound to, beside the company.
const PURPOSE: &str = "reader_key";

/// The reader a read uses: the company's own key, else the server's.
pub enum Reader<'a> {
    Shared(&'a Extractor),
    Own(Box<Extractor>),
}

impl Deref for Reader<'_> {
    type Target = Extractor;
    fn deref(&self) -> &Extractor {
        match self {
            Reader::Shared(e) => e,
            Reader::Own(e) => e,
        }
    }
}

impl Reader<'_> {
    /// What each extraction records as its provider.
    pub fn label(&self) -> String {
        match self {
            Reader::Shared(e) => e.provider_label(),
            Reader::Own(e) => format!("{} (company key)", e.provider_label()),
        }
    }
}

/// A provider a company can read with: its name in commands, what people
/// see, how Payvy talks to it, and where.
pub struct Known {
    pub name: &'static str,
    pub label: &'static str,
    pub provider: Provider,
    pub base_url: &'static str,
    /// What to suggest choosing from the provider's list.
    pub hint: &'static str,
}

pub const PROVIDERS: [Known; 5] = [
    Known {
        name: "anthropic",
        label: "Anthropic (Claude)",
        provider: Provider::Anthropic,
        base_url: "https://api.anthropic.com",
        hint: "Sonnet reads invoices well at a modest price; Haiku is cheaper, Opus is the most careful.",
    },
    Known {
        name: "openai",
        label: "OpenAI",
        provider: Provider::OpenAiCompatible,
        base_url: "https://api.openai.com/v1",
        hint: "Pick a current general GPT model (not a mini or nano for invoices with many lines).",
    },
    Known {
        name: "gemini",
        label: "Google Gemini",
        provider: Provider::OpenAiCompatible,
        base_url: "https://generativelanguage.googleapis.com/v1beta/openai",
        hint: "A current Gemini Flash model is fast and cheap for invoices; Pro is more careful.",
    },
    Known {
        name: "xai",
        label: "xAI (Grok)",
        provider: Provider::OpenAiCompatible,
        base_url: "https://api.x.ai/v1",
        hint: "Pick the newest Grok model that takes images and files.",
    },
    Known {
        name: "meta",
        label: "Meta (Muse)",
        provider: Provider::OpenAiCompatible,
        base_url: "https://api.meta.ai/v1",
        hint: "Muse Spark is the everyday model; Muse Wonder is the most capable.",
    },
];

fn known(name: &str) -> Option<&'static Known> {
    PROVIDERS.iter().find(|k| k.name == name)
}

fn unknown(name: &str) -> CommandError {
    let names: Vec<&str> = PROVIDERS.iter().map(|k| k.name).collect();
    CommandError::Invalid(format!(
        "provider is one of {}, not {name:?}",
        names.join(", ")
    ))
}

fn extractor(
    origin: Option<&str>,
    k: &Known,
    model: &str,
    key: String,
) -> Result<Extractor, CommandError> {
    Extractor::new(Config {
        provider: k.provider,
        base_url: origin.map_or_else(|| k.base_url.into(), str::to_owned),
        api_key: Some(key),
        model: model.into(),
    })
    .map_err(|e| CommandError::Invalid(e.to_string()))
}

/// The key as pasted, refused when it plainly isn't one, or is another
/// provider's.
fn clean_key(k: &Known, raw: &str) -> Result<String, CommandError> {
    let key = raw.trim().to_owned();
    if key.len() < 20 || key.len() > 400 || key.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(CommandError::Invalid(
            "that doesn't look like an API key: paste the whole key, with nothing around it".into(),
        ));
    }
    let anthropic = key.starts_with("sk-ant-");
    if k.name == "anthropic" && !anthropic {
        return Err(CommandError::Invalid(
            "that isn't an Anthropic key (they start sk-ant-); choose the provider the key is from"
                .into(),
        ));
    }
    if k.name != "anthropic" && anthropic {
        return Err(CommandError::Invalid(
            "that's an Anthropic key; choose Anthropic for it".into(),
        ));
    }
    Ok(key)
}

/// The company's own reader, when it has set a key.
pub async fn own(cx: &mut Cx<'_>) -> Result<Option<Extractor>, CommandError> {
    let Some(k) = store::reader_key(&mut cx.tx, cx.org).await? else {
        return Ok(None);
    };
    let sealer = &cx
        .bank()
        .map_err(|_| {
            CommandError::NotConfigured(
                "this company has its own model key, but the server can't unseal it (set PAYVY_SECRETS_KEY)",
            )
        })?
        .sealer;
    let key = sealer.open(cx.org, PURPOSE, &k.key_sealed)?;
    let p = known(&k.provider).ok_or(CommandError::NotConfigured(
        "this company's model key names a provider this server doesn't know",
    ))?;
    extractor(cx.reader_key_origin, p, &k.model, key).map(Some)
}

fn view(k: Option<&ReaderKey>) -> Value {
    match k {
        Some(k) => json!({
            "configured": true,
            "provider": k.provider,
            "model": k.model,
            "key_last4": k.key_last4,
            "set_at": k.set_at,
            "set_by": k.set_by,
        }),
        None => json!({ "configured": false }),
    }
}

pub async fn get(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let k = store::reader_key(&mut cx.tx, cx.org).await?;
    let mut v = view(k.as_ref());
    v["providers"] = json!(
        PROVIDERS
            .iter()
            .map(|k| json!({ "name": k.name, "label": k.label, "hint": k.hint,
                             "default_model": (k.name == "anthropic").then_some(payvy_extract::DEFAULT_CLAUDE_MODEL) }))
            .collect::<Vec<_>>()
    );
    v["builtin_reader"] = json!(cx.extractor().is_ok());
    Ok(v)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Set {
    provider: String,
    api_key: String,
    model: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Models {
    provider: String,
    api_key: String,
}

/// The models a key can use, so a person picks one the provider offers.
/// Asks the provider (free); stores nothing.
pub async fn models(cx: &mut Cx<'_>, input: Models) -> Result<Value, CommandError> {
    let k = known(&input.provider).ok_or_else(|| unknown(&input.provider))?;
    let key = clean_key(k, &input.api_key)?;
    let models = extractor(cx.reader_key_origin, k, "listing", key)?
        .models()
        .await
        .map_err(|e| CommandError::Invalid(e.to_string()))?;
    let suggested = (k.name == "anthropic"
        && models
            .iter()
            .any(|m| m == payvy_extract::DEFAULT_CLAUDE_MODEL))
    .then_some(payvy_extract::DEFAULT_CLAUDE_MODEL);
    Ok(json!({ "provider": k.name, "models": models, "suggested": suggested, "hint": k.hint }))
}

pub async fn set(cx: &mut Cx<'_>, input: Set) -> Result<Value, CommandError> {
    let p = known(&input.provider).ok_or_else(|| unknown(&input.provider))?;
    let key = clean_key(p, &input.api_key)?;
    let model = match (
        input
            .model
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty()),
        p.name,
    ) {
        (Some(m), _) => m,
        (None, "anthropic") => payvy_extract::DEFAULT_CLAUDE_MODEL.into(),
        (None, _) => {
            return Err(CommandError::Invalid(format!(
                "choose the {} model to read with",
                p.label
            )));
        }
    };
    let sealer = &cx
        .bank()
        .map_err(|_| {
            CommandError::NotConfigured("this server can't store keys (set PAYVY_SECRETS_KEY)")
        })?
        .sealer;
    // Ask the provider before storing anything: the key works, the model
    // is one it offers, and it reads a sample PDF invoice through this API.
    // A key that can't read is refused here rather than failing every
    // invoice later.
    extractor(cx.reader_key_origin, p, &model, key.clone())?
        .check()
        .await
        .map_err(|e| CommandError::Invalid(e.to_string()))?;
    let last4: String = key
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let k = ReaderKey {
        provider: input.provider.clone(),
        model,
        key_sealed: sealer.seal(cx.org, PURPOSE, &key)?,
        key_last4: last4,
        set_at: cx.now,
        set_by: cx.actor.name.clone(),
    };
    let replaced = store::reader_key(&mut cx.tx, cx.org).await?.is_some();
    store::put_reader_key(&mut cx.tx, cx.org, &k).await?;
    cx.emit(
        "reader_key.set",
        cx.org,
        json!({ "provider": k.provider, "model": k.model, "key_last4": k.key_last4, "replaced": replaced }),
    )
    .await?;
    let mut v = view(Some(&k));
    v["note"] = json!(
        "Payvy now reads this company's invoices with this key; your provider bills the reading to you. Invoices that arrive from now on are read automatically; read earlier ones from Documents."
    );
    Ok(v)
}

pub async fn remove(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let k = store::reader_key(&mut cx.tx, cx.org)
        .await?
        .ok_or(CommandError::NotFound("model key"))?;
    store::delete_reader_key(&mut cx.tx, cx.org).await?;
    cx.emit(
        "reader_key.removed",
        cx.org,
        json!({ "provider": k.provider, "model": k.model, "key_last4": k.key_last4 }),
    )
    .await?;
    Ok(
        json!({ "configured": false, "note": "Removed. Payvy no longer reads with this key; delete it at the provider too if it isn't used elsewhere." }),
    )
}
