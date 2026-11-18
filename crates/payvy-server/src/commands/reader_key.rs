//! A company's own model key for Payvy's reader (Settings > Agents >
//! Reading). With one, `document.extract` and the background reader read the
//! company's invoices with its key, billed to the company by its provider,
//! whatever its plan's built-in reader. The key is checked with the provider
//! (a free model lookup), sealed with PAYVY_SECRETS_KEY, and never returned,
//! logged or put in an event: only its last four characters are kept in the
//! clear. The provider's address is fixed per provider, so a key is only ever
//! sent to Anthropic or OpenAI.

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

fn provider(name: &str) -> Option<Provider> {
    match name {
        "anthropic" => Some(Provider::Anthropic),
        "openai" => Some(Provider::OpenAiCompatible),
        _ => None,
    }
}

fn extractor(
    origin: Option<&str>,
    provider: Provider,
    model: &str,
    key: String,
) -> Result<Extractor, CommandError> {
    Extractor::new(Config {
        provider,
        base_url: origin.map_or_else(|| provider.default_base_url().into(), str::to_owned),
        api_key: Some(key),
        model: model.into(),
    })
    .map_err(|e| CommandError::Invalid(e.to_string()))
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
    let p = provider(&k.provider).ok_or(CommandError::NotConfigured(
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
    v["default_models"] = json!({ "anthropic": payvy_extract::DEFAULT_CLAUDE_MODEL });
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

pub async fn set(cx: &mut Cx<'_>, input: Set) -> Result<Value, CommandError> {
    let p = provider(&input.provider).ok_or_else(|| {
        CommandError::Invalid(format!(
            "provider is anthropic or openai, not {:?}",
            input.provider
        ))
    })?;
    let key = input.api_key.trim().to_owned();
    if key.len() < 20 || key.len() > 400 || key.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(CommandError::Invalid(
            "that doesn't look like an API key: paste the whole key, with nothing around it".into(),
        ));
    }
    match p {
        Provider::Anthropic if !key.starts_with("sk-ant-") => {
            return Err(CommandError::Invalid(
                "that isn't an Anthropic key (they start sk-ant-); choose OpenAI for an OpenAI key"
                    .into(),
            ));
        }
        Provider::OpenAiCompatible if key.starts_with("sk-ant-") => {
            return Err(CommandError::Invalid(
                "that's an Anthropic key; choose Anthropic for it".into(),
            ));
        }
        _ => {}
    }
    let model = match (
        input
            .model
            .map(|m| m.trim().to_owned())
            .filter(|m| !m.is_empty()),
        p,
    ) {
        (Some(m), _) => m,
        (None, Provider::Anthropic) => payvy_extract::DEFAULT_CLAUDE_MODEL.into(),
        (None, Provider::OpenAiCompatible) => {
            return Err(CommandError::Invalid(
                "name the OpenAI model to read with (one that reads PDFs and images)".into(),
            ));
        }
    };
    let sealer = &cx
        .bank()
        .map_err(|_| {
            CommandError::NotConfigured("this server can't store keys (set PAYVY_SECRETS_KEY)")
        })?
        .sealer;
    // Ask the provider before storing anything: a key that doesn't work is
    // refused here rather than failing every invoice later.
    extractor(cx.reader_key_origin, p, &model, key.clone())?
        .check_key()
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
