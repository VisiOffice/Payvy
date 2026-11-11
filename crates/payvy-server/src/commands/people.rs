//! Who can sign in to the web app. A person signs in with a link sent to
//! their email and acts as the actor of the same name.

use payvy_store::{self as store, ActorKind, NewPerson};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CommandError, Cx, text};

pub async fn list(cx: &mut Cx<'_>) -> Result<Value, CommandError> {
    Ok(json!({ "people": store::people(&mut cx.tx, cx.org).await? }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Add {
    email: String,
    name: String,
    actor: Option<String>,
}

fn valid_actor(a: &str) -> bool {
    let mut chars = a.chars();
    a.len() <= 64
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

pub async fn add(cx: &mut Cx<'_>, input: Add) -> Result<Value, CommandError> {
    let email = input.email.trim().to_lowercase();
    let Some((local, domain)) = email.split_once('@') else {
        return Err(CommandError::Invalid("email needs an @".into()));
    };
    if local.is_empty() || domain.is_empty() || email.contains(char::is_whitespace) {
        return Err(CommandError::Invalid(format!(
            "{email} isn't an email address"
        )));
    }
    let name = text(Some(input.name))
        .ok_or_else(|| CommandError::Invalid("name can't be empty".into()))?;
    let actor = match text(input.actor) {
        Some(a) => a.to_lowercase(),
        None => local.split('+').next().unwrap_or(local).to_owned(),
    };
    if !valid_actor(&actor) {
        return Err(CommandError::Invalid(format!(
            "actor {actor:?} must be a-z 0-9 . _ - (up to 64), starting with a letter or digit; pass actor to choose one"
        )));
    }
    let person = store::insert_person(
        &mut cx.tx,
        &NewPerson {
            org_id: cx.org,
            actor: &actor,
            email: &email,
            name: &name,
            created_at: cx.now,
            created_by: &cx.actor.name,
        },
    )
    .await?
    .ok_or_else(|| {
        CommandError::Conflict(format!(
            "someone here already signs in as {actor} or with {email}; pass a different actor, or person.remove them first"
        ))
    })?;
    cx.emit(
        "person.added",
        person.id,
        json!({ "actor": person.actor, "email": person.email, "name": person.name }),
    )
    .await?;
    Ok(json!({
        "person": person,
        "next": "they open the web app and ask for a sign-in link with this email",
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Remove {
    actor: String,
}

pub async fn remove(cx: &mut Cx<'_>, input: Remove) -> Result<Value, CommandError> {
    let actor = input.actor.trim().to_lowercase();
    let people = store::lock_people(&mut cx.tx, cx.org).await?;
    if !people.contains(&actor) {
        return Err(CommandError::NotFound("person"));
    }
    if cx.actor.kind == ActorKind::Person && cx.actor.name == actor {
        return Err(CommandError::Forbidden(
            "you can't remove yourself; ask someone else who signs in here".into(),
        ));
    }
    if people.len() <= 1 {
        return Err(CommandError::Conflict(format!(
            "{actor} is the last person who can sign in; add someone else first"
        )));
    }
    let id = store::remove_person(&mut cx.tx, cx.org, &actor, cx.now, &cx.actor.name)
        .await?
        .ok_or(CommandError::NotFound("person"))?;
    cx.emit("person.removed", id, json!({ "actor": actor }))
        .await?;
    Ok(
        json!({ "removed": actor, "note": "signed out everywhere; their past actions stay on the record" }),
    )
}

#[cfg(test)]
mod tests {
    use super::valid_actor;

    #[test]
    fn actor_names() {
        assert!(valid_actor("robert"));
        assert!(valid_actor("r.oneill-2"));
        assert!(!valid_actor(""));
        assert!(!valid_actor("-robert"));
        assert!(!valid_actor("Robert"));
        assert!(!valid_actor("rob ert"));
    }
}
