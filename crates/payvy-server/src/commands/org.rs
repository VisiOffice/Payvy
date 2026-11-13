//! The organization itself: its name, shown in the app and in sign-in emails.

use payvy_store as store;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CommandError, Cx};

/// Longest name, in characters.
const NAME_MAX: usize = 100;

pub async fn get(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    let org = store::organization(&mut cx.tx, cx.org)
        .await?
        .ok_or(CommandError::NotFound("organization"))?;
    // What the company's plan includes, so the app can say so before a
    // command is refused. The server still checks on every command.
    let stored = if cx.hosted {
        store::entitlement(&mut cx.tx, cx.org).await?
    } else {
        None
    };
    let plan = crate::entitlements::view(cx.hosted, stored.as_ref());
    Ok(json!({ "organization": org, "plan": plan }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    name: String,
}

/// Trimmed, not empty, at most NAME_MAX characters, no control characters.
pub(crate) fn valid_name(name: &str) -> Result<String, CommandError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(CommandError::Invalid("name can't be empty".into()));
    }
    if name.chars().count() > NAME_MAX {
        return Err(CommandError::Invalid(format!(
            "name can be at most {NAME_MAX} characters"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(CommandError::Invalid(
            "name can't contain line breaks or control characters".into(),
        ));
    }
    Ok(name.to_owned())
}

pub async fn update(cx: &mut Cx<'_>, input: Update) -> Result<Value, CommandError> {
    let name = valid_name(&input.name)?;
    let current = store::lock_organization(&mut cx.tx, cx.org)
        .await?
        .ok_or(CommandError::NotFound("organization"))?;
    if current.name == name {
        return Err(CommandError::Conflict(format!(
            "the organization is already named {name}"
        )));
    }
    let org = store::rename_organization(&mut cx.tx, cx.org, &name).await?;
    cx.emit(
        "org.renamed",
        org.id,
        json!({ "from": current.name, "to": org.name }),
    )
    .await?;
    Ok(json!({ "organization": org }))
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    #[test]
    fn names() {
        assert_eq!(valid_name("  Acme Co  ").unwrap(), "Acme Co");
        assert!(valid_name("   ").is_err());
        assert!(valid_name("Acme\nCo").is_err());
        assert!(valid_name(&"é".repeat(100)).is_ok());
        assert!(valid_name(&"a".repeat(101)).is_err());
    }
}
