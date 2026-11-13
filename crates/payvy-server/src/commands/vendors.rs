use payvy_store::{self as store, ActorKind};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, text, workflows};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    name: String,
    email: Option<String>,
    terms_days: Option<i32>,
}

pub async fn create(cx: &mut Cx<'_>, input: Create) -> Result<Value, CommandError> {
    let name = text(Some(input.name))
        .ok_or_else(|| CommandError::Invalid("name can't be empty".into()))?;
    if input.terms_days.is_some_and(|d| !(0..=365).contains(&d)) {
        return Err(CommandError::Invalid("terms_days is 0 to 365".into()));
    }
    let v = store::insert_vendor(
        &mut cx.tx,
        cx.org,
        &name,
        text(input.email).as_deref(),
        input.terms_days,
    )
    .await?;
    cx.emit(
        "vendor.created",
        v.id,
        json!({ "vendor_id": v.id, "name": v.name }),
    )
    .await?;
    Ok(json!(v))
}

pub async fn list(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    Ok(json!({ "vendors": store::list_vendors(&mut cx.tx).await? }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Get {
    vendor_id: Uuid,
}

pub async fn get(cx: &mut Cx<'_>, input: Get) -> Result<Value, CommandError> {
    let v = store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let accounts = store::vendor_accounts(&mut cx.tx, v.id).await?;
    let workflow = workflows::current(cx, Some(v.id)).await?;
    let payees = store::vendor_payees(&mut cx.tx, v.id).await?;
    let current_bank = store::live_paying_connection(&mut cx.tx).await?;
    let payees: Vec<Value> = payees
        .into_iter()
        .map(|p| {
            let is_current = current_bank
                .as_ref()
                .is_some_and(|c| c.id == p.connection_id);
            let mut value = json!(p);
            value["is_current_connection"] = json!(is_current);
            value
        })
        .collect();
    let limits = store::auto_pay_limits(&mut cx.tx).await?;
    let auto_pay_limit = json!({
        "own_cents": limits.vendors.get(&v.id),
        "in_force": limits.for_vendor(v.id),
        "company_cents": payvy_domain::workflow::AutoPayLimit::effective(limits.company, None).cents,
    });
    Ok(
        json!({ "vendor": v, "accounts": accounts, "workflow": workflow, "payees": payees, "payee_invites": store::vendor_payee_invites(&mut cx.tx, v.id).await?, "auto_pay_limit": auto_pay_limit }),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AddAccount {
    vendor_id: Uuid,
    account_number: String,
    location: Option<String>,
}

pub async fn add_account(cx: &mut Cx<'_>, input: AddAccount) -> Result<Value, CommandError> {
    store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let number = text(Some(input.account_number))
        .ok_or_else(|| CommandError::Invalid("account_number can't be empty".into()))?;
    let account = store::insert_vendor_account(
        &mut cx.tx,
        cx.org,
        input.vendor_id,
        &number,
        text(input.location).as_deref(),
    )
    .await?
    .ok_or_else(|| {
        CommandError::Conflict(format!("this vendor already has account number {number}"))
    })?;
    cx.emit(
        "vendor.account_added",
        input.vendor_id,
        json!({ "vendor_id": input.vendor_id, "account_number": account.account_number, "location": account.location }),
    )
    .await?;
    Ok(json!(account))
}

/// Tells a field left out (keep it) from one sent as null (clear it).
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    vendor_id: Uuid,
    name: Option<String>,
    #[serde(default, deserialize_with = "present")]
    email: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    terms_days: Option<Option<i32>>,
}

/// Changes a vendor's name, email or payment terms. Name and email decide
/// which vendor an incoming invoice matches, and email is where payment-detail
/// invitations go, so only a person changes those; an agent may change terms.
pub async fn update(cx: &mut Cx<'_>, input: Update) -> Result<Value, CommandError> {
    let current = store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let name = match input.name {
        Some(n) => {
            text(Some(n)).ok_or_else(|| CommandError::Invalid("name can't be empty".into()))?
        }
        None => current.name.clone(),
    };
    let email = match input.email {
        Some(e) => text(e),
        None => current.email.clone(),
    };
    if let Some(e) = &email
        && (!e.contains('@') || e.contains(char::is_whitespace))
    {
        return Err(CommandError::Invalid(format!("{e} isn't an email address")));
    }
    let terms_days = input.terms_days.unwrap_or(current.terms_days);
    if terms_days.is_some_and(|d| !(0..=365).contains(&d)) {
        return Err(CommandError::Invalid("terms_days is 0 to 365".into()));
    }

    let mut changes = serde_json::Map::new();
    if name != current.name {
        changes.insert("name".into(), json!({ "from": current.name, "to": name }));
    }
    if email != current.email {
        changes.insert(
            "email".into(),
            json!({ "from": current.email, "to": email }),
        );
    }
    if terms_days != current.terms_days {
        changes.insert(
            "terms_days".into(),
            json!({ "from": current.terms_days, "to": terms_days }),
        );
    }
    if changes.is_empty() {
        return Ok(json!(current));
    }
    if cx.actor.kind != ActorKind::Person
        && (changes.contains_key("name") || changes.contains_key("email"))
    {
        return Err(CommandError::Forbidden(
            "only a person can change a vendor's name or email: they decide which invoices match it and where payment-detail invitations go".into(),
        ));
    }
    let v = store::update_vendor(&mut cx.tx, current.id, &name, email.as_deref(), terms_days)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    cx.emit(
        "vendor.updated",
        v.id,
        json!({ "vendor_id": v.id, "changes": changes }),
    )
    .await?;
    Ok(json!(v))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoveAccount {
    vendor_id: Uuid,
    account_id: Uuid,
}

pub async fn remove_account(cx: &mut Cx<'_>, input: RemoveAccount) -> Result<Value, CommandError> {
    let account = store::delete_vendor_account(&mut cx.tx, input.vendor_id, input.account_id)
        .await?
        .ok_or(CommandError::NotFound("account number"))?;
    cx.emit(
        "vendor.account_removed",
        input.vendor_id,
        json!({ "vendor_id": input.vendor_id, "account_number": account.account_number, "location": account.location }),
    )
    .await?;
    Ok(json!(account))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkExternal {
    vendor_id: Uuid,
    source: String,
    external_id: String,
}

/// Link a Payvy vendor to its id in another system, once per source.
/// The same link again is a no-op. A different id, or another vendor
/// already holding this id, is refused.
pub async fn link_external(cx: &mut Cx<'_>, input: LinkExternal) -> Result<Value, CommandError> {
    let source = text(Some(input.source))
        .ok_or_else(|| CommandError::Invalid("source is required".into()))?;
    let external_id = text(Some(input.external_id))
        .ok_or_else(|| CommandError::Invalid("external_id is required".into()))?;
    store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let links = store::purchase_orders::vendor_links_touching(
        &mut cx.tx,
        &source,
        input.vendor_id,
        &external_id,
    )
    .await?;
    let same = links
        .iter()
        .any(|l| l.vendor_id == input.vendor_id && l.external_id == external_id);
    if links
        .iter()
        .any(|l| l.vendor_id == input.vendor_id && l.external_id != external_id)
    {
        return Err(CommandError::Conflict(
            "this vendor is already linked to a different id for that source".into(),
        ));
    }
    if links.iter().any(|l| l.vendor_id != input.vendor_id) {
        return Err(CommandError::Conflict(
            "that external id is already linked to another vendor".into(),
        ));
    }
    if !same {
        store::purchase_orders::insert_vendor_link(
            &mut cx.tx,
            cx.org,
            input.vendor_id,
            &source,
            &external_id,
            &cx.actor.name,
            cx.now,
        )
        .await?;
        cx.emit(
            "vendor.external_linked",
            input.vendor_id,
            json!({ "vendor_id": input.vendor_id, "source": source, "external_id": external_id }),
        )
        .await?;
    }
    Ok(json!({
        "vendor_id": input.vendor_id,
        "source": source,
        "external_id": external_id,
        "outcome": if same { "unchanged" } else { "linked" },
    }))
}
