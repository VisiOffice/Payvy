use payvy_store as store;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct List {
    entity_id: Option<Uuid>,
    limit: Option<i64>,
}

pub async fn list(cx: &mut Cx<'_>, input: List) -> Result<Value, CommandError> {
    let limit = input.limit.unwrap_or(100).clamp(1, 500);
    Ok(json!({ "events": store::list_events(&mut cx.tx, input.entity_id, limit).await? }))
}
