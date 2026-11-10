//! A person initiates an invitation. Mercury collects the banking details;
//! completing it proposes a payee, and never accepts or pays one.
use payvy_pay::{BankError, Method};
use payvy_store::{self as store, NewPayee, PayeeInvite};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{CommandError, Cx, bank, text};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Invite {
    vendor_id: Uuid,
    email: String,
    method: String,
}

pub async fn invite(cx: &mut Cx<'_>, input: Invite) -> Result<Value, CommandError> {
    let email = input.email.trim();
    if email.len() > 254
        || email.chars().any(char::is_whitespace)
        || !email.split_once('@').is_some_and(|(local, domain)| {
            !local.is_empty() && domain.contains('.') && !domain.contains('@')
        })
    {
        return Err(CommandError::Invalid(
            "enter the vendor's email address".into(),
        ));
    }
    let method = Method::parse(&input.method)
        .ok_or_else(|| CommandError::Invalid("choose a supported payment method".into()))?;
    if !store::lock_invite_vendor(&mut cx.tx, input.vendor_id).await? {
        return Err(CommandError::NotFound("vendor"));
    }
    let vendor = store::get_vendor(&mut cx.tx, input.vendor_id)
        .await?
        .ok_or(CommandError::NotFound("vendor"))?;
    let c = bank::live(cx).await?;
    if c.provider != "mercury" {
        return Err(CommandError::Invalid("invitations require Mercury".into()));
    }
    let existing = store::vendor_payee_invites(&mut cx.tx, vendor.id).await?;
    if existing.iter().any(|i| {
        i.connection_id == c.id
            && (matches!(i.status.as_str(), "unknown" | "created")
                || (i.status == "completed" && i.proposal_id.is_none()))
    }) {
        return Err(CommandError::Conflict(
            "this vendor already has an open invitation; check it before sending another".into(),
        ));
    }
    let client = bank::mercury(cx, &c)?;
    if cx.preview {
        return Ok(
            json!({ "vendor_name": vendor.name, "email": email, "method": method,
            "next": "Mercury emails the vendor; a person accepts the payee after the vendor completes the invitation" }),
        );
    }
    let mut invite = PayeeInvite {
        id: Uuid::new_v4(),
        vendor_id: vendor.id,
        connection_id: c.id,
        contact_email: email.into(),
        method: method.as_str().into(),
        status: "unknown".into(),
        mercury_invite_id: None,
        proposal_id: None,
        created_at: cx.now,
        created_by: cx.actor.name.clone(),
    };
    store::insert_payee_invite(&mut cx.tx, cx.org, &invite).await?;
    cx.emit(
        "vendor.payee_invite_started",
        vendor.id,
        json!({ "invite_id": invite.id, "email": email, "method": method }),
    )
    .await?;
    // Consume the confirmation and durably record an uncertain attempt BEFORE
    // the non-idempotent POST. A crash after this point must never resend it.
    // The replacement transaction remains organization-scoped, like every write.
    let next = cx.store.begin(cx.org).await?;
    let previous = std::mem::replace(&mut cx.tx, next);
    previous.commit().await.map_err(anyhow::Error::from)?;
    // Hold this row through the response so a simultaneous refresh waits.
    store::get_payee_invite(&mut cx.tx, invite.id).await?;
    let note = match client.invite_recipient(&vendor.name, email, method).await {
        Ok(remote) => {
            invite.status = remote.status;
            invite.mercury_invite_id = Some(remote.id);
            "Mercury created the invitation. Check its status after the vendor responds."
        }
        Err(BankError::Refused(_) | BankError::Unauthorized(_)) => {
            invite.status = "failed".into();
            "Mercury refused the invitation. Check the connection's recipient permissions and email before trying again."
        }
        Err(BankError::Unclear(_)) => {
            "Delivery is uncertain. Check Mercury before sending another invitation; enter its invitation ID to recover it here."
        }
    };
    store::update_payee_invite(&mut cx.tx, &invite).await?;
    cx.emit(
        "vendor.payee_invite_recorded",
        vendor.id,
        json!({ "invite_id": invite.id, "status": invite.status }),
    )
    .await?;
    Ok(json!({ "invite": invite, "note": note }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Refresh {
    invite_id: Uuid,
    mercury_invite_id: Option<String>,
}

pub async fn refresh(cx: &mut Cx<'_>, input: Refresh) -> Result<Value, CommandError> {
    let mut invite = store::get_payee_invite(&mut cx.tx, input.invite_id)
        .await?
        .ok_or(CommandError::NotFound("invitation"))?;
    let c = bank::live(cx).await?;
    if c.id != invite.connection_id {
        return Err(CommandError::Conflict("this invitation belongs to a replaced bank connection; invite the vendor on the current connection".into()));
    }
    if invite.proposal_id.is_some()
        || matches!(invite.status.as_str(), "expired" | "declined" | "failed")
    {
        return Err(CommandError::Conflict(
            "this invitation has already been resolved".into(),
        ));
    }
    let supplied_id = text(input.mercury_invite_id);
    if supplied_id.is_some() && invite.mercury_invite_id.is_some() {
        return Err(CommandError::Invalid(
            "the Mercury invitation ID is already known".into(),
        ));
    }
    let remote_id = invite
        .mercury_invite_id
        .as_ref()
        .or(supplied_id.as_ref())
        .ok_or_else(|| {
            CommandError::Invalid(
                "find the invitation in Mercury and enter its invitation ID to recover it".into(),
            )
        })?;
    let client = bank::mercury(cx, &c)?;
    let remote = client.recipient_invite(remote_id).await?;
    let method = Method::parse(&invite.method)
        .ok_or_else(|| anyhow::anyhow!("invalid stored invite method"))?;
    if remote.id != *remote_id
        || !remote
            .contact_email
            .eq_ignore_ascii_case(&invite.contact_email)
        || !remote.payment_methods.iter().any(|m| m == method.mercury())
    {
        return Err(CommandError::Conflict(
            "Mercury invitation does not match this email and payment method".into(),
        ));
    }
    let changed = invite.status != remote.status || invite.mercury_invite_id.is_none();
    if remote.status == "completed" {
        let recipient_id = remote
            .recipient_id
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                CommandError::Bank(
                    "Mercury has not supplied the completed recipient yet; check again later"
                        .into(),
                )
            })?;
        let recipient = client
            .recipient_by_id(recipient_id)
            .await.map_err(|_| CommandError::Bank("could not read the invited recipient from Mercury; check the connection and try again".into()))?
            .ok_or(CommandError::NotFound("recipient in Mercury"))?;
        if !recipient.active {
            return Err(CommandError::Conflict(
                "the invited recipient is no longer active in Mercury".into(),
            ));
        }
        let destination = recipient.destination(method).ok_or_else(|| {
            CommandError::Conflict(
                "the recipient has not supplied details for the invited payment method".into(),
            )
        })?;
        let proposal = store::insert_payee_proposal(
            &mut cx.tx,
            cx.org,
            &NewPayee {
                vendor_id: invite.vendor_id,
                connection_id: c.id,
                recipient_id: &recipient.id,
                recipient_name: &recipient.name,
                method: method.as_str(),
                destination,
                reason: Some("Vendor completed a Mercury invitation"),
                proposed_by: &cx.actor.name,
                created_at: cx.now,
            },
        )
        .await?;
        invite.proposal_id = Some(proposal.id);
        cx.emit("vendor.payee_proposed", invite.vendor_id, json!({ "vendor_id": invite.vendor_id,
            "proposal_id": proposal.id, "recipient_name": proposal.recipient_name, "method": proposal.method,
            "destination": proposal.destination, "invite_id": invite.id })).await?;
    } else if !changed {
        return Err(CommandError::Conflict(
            "the vendor has not responded yet; the invitation is unchanged".into(),
        ));
    }
    invite.status = remote.status;
    invite.mercury_invite_id = Some(remote.id);
    store::update_payee_invite(&mut cx.tx, &invite).await?;
    cx.emit("vendor.payee_invite_updated", invite.vendor_id,
        json!({ "invite_id": invite.id, "status": invite.status, "proposal_id": invite.proposal_id })).await?;
    Ok(
        json!({ "invite": invite, "next": "a person reviews any proposal with vendor.accept_payee" }),
    )
}
