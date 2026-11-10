//! The command registry. Every action in Payvy is one of these commands, and
//! the web app, REST API and MCP server are all callers of it. Nothing is
//! UI-only, and nothing writes around it.

use serde::Serialize;

/// How much ceremony a command needs before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskTier {
    /// Runs freely.
    Read,
    /// Runs and is logged.
    Routine,
    /// Money and irreversible actions: `preview` returns the exact effect and
    /// a token bound to it; `commit` must quote that token.
    Confirmed,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub struct CommandSpec {
    pub name: &'static str,
    pub summary: &'static str,
    pub tier: RiskTier,
    /// Only a person may run it: agents can prepare the ground (propose a
    /// workflow, gather an approval packet) but not sign off.
    pub person_only: bool,
    /// JSON Schema for the input, served as-is to MCP clients.
    pub input_schema: &'static str,
}

/// One workflow step, as JSON Schema. Kept in step with `workflow::StepKind`
/// by a test.
macro_rules! step_schema {
    () => {
        r#"{"type":"object","required":["id","type"],"properties":{"id":{"type":"string","description":"Stable step id, a-z 0-9 _ -"},"type":{"type":"string","enum":["approve","hold","wait_for","pay"]},"approvers":{"type":"array","items":{"type":"string"},"description":"approve: people who may sign off; empty means anyone"},"require":{"type":"string","enum":["any","all"],"description":"approve: one approver or every one"},"min_amount_cents":{"type":"integer","minimum":0,"description":"approve: bills under this skip the step"},"days":{"type":"integer","minimum":1,"maximum":365,"description":"hold: days to wait after the previous step"},"signal":{"type":"string","description":"wait_for: the signal key, e.g. customer_funded"},"label":{"type":"string","description":"wait_for: what the signal means, for people"},"release":{"type":"string","enum":["manual","auto"],"description":"pay: wait for a person to release, or pay as soon as the bill gets here"},"by":{"type":"array","items":{"type":"string"},"description":"pay (manual): who may release; empty means anyone"}}}"#
    };
}

pub const COMMANDS: &[CommandSpec] = &[
    // ── Vendors ─────────────────────────────────────────────────────────────
    CommandSpec {
        name: "vendor.create",
        summary: "Create a vendor.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["name"],"properties":{"name":{"type":"string"},"email":{"type":"string","format":"email"},"terms_days":{"type":"integer","minimum":0,"maximum":365,"description":"Payment terms, e.g. 30 for net 30"}}}"#,
    },
    CommandSpec {
        name: "vendor.list",
        summary: "List vendors with their open bill count and amount.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "vendor.get",
        summary: "A vendor with its account numbers, the workflow its bills follow and any open workflow proposals.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","required":["vendor_id"],"properties":{"vendor_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "vendor.add_account",
        summary: "Record an account number this vendor uses for you, optionally tied to a location (a job site, a store). Invoices quoting it are matched to this vendor and location.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["vendor_id","account_number"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"account_number":{"type":"string"},"location":{"type":"string","description":"e.g. 1420 Elm St site"}}}"#,
    },
    // ── Workflows ───────────────────────────────────────────────────────────
    CommandSpec {
        name: "workflow.get",
        summary: "The workflow a vendor's bills follow (or, without vendor_id, the organization default): its steps, a Mermaid chart, where it comes from, and open proposals.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{"vendor_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "workflow.propose",
        summary: "Propose a workflow for a vendor, or without vendor_id for the organization default. Nothing changes until a person accepts it with workflow.accept. Steps run in order and the last is the only pay step. Bills already received keep the workflow they arrived under.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: concat!(
            r#"{"type":"object","required":["workflow"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"reason":{"type":"string","description":"Why, for the person reviewing it"},"workflow":{"type":"object","required":["steps"],"properties":{"steps":{"type":"array","minItems":1,"items":"#,
            step_schema!(),
            r#"}}}}}"#
        ),
    },
    CommandSpec {
        name: "workflow.accept",
        summary: "Accept a workflow proposal. It applies to bills received from now on.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","required":["proposal_id"],"properties":{"proposal_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "workflow.decline",
        summary: "Decline a workflow proposal.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","required":["proposal_id"],"properties":{"proposal_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    // ── Bills ───────────────────────────────────────────────────────────────
    CommandSpec {
        name: "bill.create",
        summary: "Enter a bill. It follows the vendor's current workflow from now on. Refused if this vendor already has a bill with the same invoice number (ignoring case, spaces, punctuation and leading zeros) unless allow_duplicate is true.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["vendor_id","invoice_number","invoice_date","amount_cents"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"invoice_number":{"type":"string"},"invoice_date":{"type":"string","format":"date"},"due_date":{"type":"string","format":"date","description":"Defaults to invoice_date plus the vendor's terms"},"amount_cents":{"type":"integer","minimum":1},"memo":{"type":"string"},"external_ref":{"type":"string","description":"Your own id for this bill (a PO or order number), so other systems can signal it"},"lines":{"type":"array","items":{"type":"object","required":["description","amount_cents"],"properties":{"description":{"type":"string"},"amount_cents":{"type":"integer","minimum":1},"account":{"type":"string"}}},"description":"If given, they must add up to amount_cents"},"allow_duplicate":{"type":"boolean"}}}"#,
    },
    CommandSpec {
        name: "bill.list",
        summary: "List bills, newest first. Filter by state (open, rejected, void, paid), by where open bills stand (status: awaiting_approval, held, waiting_for, awaiting_release, ready_to_pay, released), by vendor, or to bills waiting on you (mine).",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{"state":{"type":"string","enum":["open","rejected","void","paid"]},"status":{"type":"string","enum":["awaiting_approval","held","waiting_for","awaiting_release","ready_to_pay","released"]},"vendor_id":{"type":"string","format":"uuid"},"mine":{"type":"boolean","description":"Only bills a step is waiting on you for"},"limit":{"type":"integer","minimum":1,"maximum":500}}}"#,
    },
    CommandSpec {
        name: "bill.get",
        summary: "A bill with its lines, its workflow progress step by step, approvals, signals and history.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bill.approve",
        summary: "Approve a bill at its current approval step. You must be one of that step's approvers.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"},"note":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "bill.reject",
        summary: "Reject a bill: it will not be paid. Final.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","required":["bill_id","reason"],"properties":{"bill_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "bill.signal",
        summary: "Report that something outside Payvy happened for a bill, e.g. customer_funded when the customer has paid for this cost. A workflow's wait_for step waits for its signal. Name the bill by bill_id or by the external_ref it was created with.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["signal"],"properties":{"bill_id":{"type":"string","format":"uuid"},"external_ref":{"type":"string"},"signal":{"type":"string","description":"a-z 0-9 _, e.g. customer_funded"},"note":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "bill.release",
        summary: "Release a bill for payment at its pay step. Preview first; commit with the returned token.",
        tier: RiskTier::Confirmed,
        person_only: true,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bill.mark_paid",
        summary: "Record that a released or ready bill was paid outside Payvy (a check, a payment made in the bank's own app). Preview first; commit with the returned token.",
        tier: RiskTier::Confirmed,
        person_only: false,
        input_schema: r#"{"type":"object","required":["bill_id","paid_on","method"],"properties":{"bill_id":{"type":"string","format":"uuid"},"paid_on":{"type":"string","format":"date"},"method":{"type":"string","enum":["ach","check","wire","card","other"]},"reference":{"type":"string","description":"Check number, transfer id"}}}"#,
    },
    CommandSpec {
        name: "bill.void",
        summary: "Void a bill entered by mistake. It no longer counts for duplicate detection. Preview first; commit with the returned token.",
        tier: RiskTier::Confirmed,
        person_only: true,
        input_schema: r#"{"type":"object","required":["bill_id","reason"],"properties":{"bill_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    // ── Audit ───────────────────────────────────────────────────────────────
    CommandSpec {
        name: "event.list",
        summary: "The audit log, newest first: every change, who made it and when. Filter to one bill, vendor or proposal with entity_id.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{"entity_id":{"type":"string","format":"uuid"},"limit":{"type":"integer","minimum":1,"maximum":500}}}"#,
    },
];

pub fn find(name: &str) -> Option<&'static CommandSpec> {
    COMMANDS.iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    #[test]
    fn names_are_unique() {
        for (i, a) in COMMANDS.iter().enumerate() {
            assert!(
                COMMANDS[i + 1..].iter().all(|b| b.name != a.name),
                "duplicate command {}",
                a.name
            );
        }
    }

    #[test]
    fn every_input_schema_is_a_json_object_schema() {
        for c in COMMANDS {
            let v: Value =
                serde_json::from_str(c.input_schema).unwrap_or_else(|e| panic!("{}: {e}", c.name));
            assert_eq!(v["type"], "object", "{}", c.name);
        }
    }

    #[test]
    fn reads_are_never_person_only() {
        for c in COMMANDS.iter().filter(|c| c.tier == RiskTier::Read) {
            assert!(!c.person_only, "{}", c.name);
        }
    }

    #[test]
    fn step_schema_matches_the_domain() {
        let v: Value =
            serde_json::from_str(find("workflow.propose").unwrap().input_schema).unwrap();
        let step = &v["properties"]["workflow"]["properties"]["steps"]["items"];
        let types: Vec<&str> = step["properties"]["type"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t.as_str().unwrap())
            .collect();
        // Every listed type parses as a step with only its own fields...
        for (ty, extra) in [
            ("approve", serde_json::json!({})),
            ("hold", serde_json::json!({"days": 1})),
            ("wait_for", serde_json::json!({"signal": "x"})),
            ("pay", serde_json::json!({})),
        ] {
            assert!(types.contains(&ty), "{ty} missing from the schema");
            let mut s = extra;
            s["id"] = "a".into();
            s["type"] = ty.into();
            serde_json::from_value::<crate::workflow::Step>(s)
                .unwrap_or_else(|e| panic!("{ty}: {e}"));
        }
        // ...and nothing else is listed.
        assert_eq!(types.len(), 4);
    }
}
