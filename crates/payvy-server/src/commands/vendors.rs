use payvy_store as store;
use serde::Deserialize;
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
    Ok(json!({ "vendor": v, "accounts": accounts, "workflow": workflow }))
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
