//! The command registry. Every action in Payvy is one of these commands, and
//! the web app, REST API and MCP server are all callers of it. Nothing is
//! UI-only, and nothing writes around it.

use crate::permissions::Permission;
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
    pub permission: Permission,
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
    CommandSpec {
        name: "invoice_review.settings",
        summary: "Read whether new document line reviews are enabled.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":[],"properties":{}}"#,
    },
    CommandSpec {
        name: "invoice_review.configure",
        summary: "Enable line review for new documents. Turning it off retains validation on previously reviewed documents.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":["enabled"],"properties":{"enabled":{"type":"boolean"},"extraction_v2_enabled":{"type":"boolean","description":"Opt in to richer extraction with source checks. Requires line review enabled. Off by default."}}}"#,
    },
    CommandSpec {
        name: "document.review_get",
        summary: "Read review revisions, immutable extraction seed and validation, independent of vendor matching.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":["document_id"],"properties":{"document_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "document.review_save",
        summary: "Save an incomplete or complete invoice review. Preserve source rows; exclude with a reason. Request IDs replay identical writes; stale versions conflict. Edits invalidate summary acceptance.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":["document_id","extraction_id","expected_version","arrangement_version","request_id","payload"],"properties":{"document_id":{"type":"string","format":"uuid"},"extraction_id":{"type":["string","null"],"format":"uuid"},"expected_version":{"type":"integer","minimum":0},"arrangement_version":{"type":"integer","minimum":0},"request_id":{"type":"string","format":"uuid"},"payload":{"type":"object","additionalProperties":false,"required":["vendor_id","invoice_number","invoice_date","due_date","total","currency","memo","rows","tax","tax_basis","source_checked"],"properties":{"vendor_id":{"type":["string","null"]},"invoice_number":{"type":"string"},"invoice_date":{"type":["string","null"]},"due_date":{"type":["string","null"]},"total":{"type":"string"},"currency":{"type":"string"},"memo":{"type":"string"},"tax":{"type":"string"},"tax_basis":{"type":"string","enum":["included","additional","unknown"]},"source_checked":{"type":"boolean"},"rows":{"type":"array","maxItems":1000,"items":{"type":"object","additionalProperties":false,"required":["id","description","amount","source_index","excluded_reason"],"properties":{"id":{"type":"string"},"description":{"type":"string"},"amount":{"type":"string"},"source_index":{"type":["integer","null"],"minimum":0},"excluded_reason":{"type":["string","null"]},"details":{"type":["object","null"],"additionalProperties":false,"properties":{"quantity":{"type":["string","null"]},"unit":{"type":["string","null"]},"unit_price":{"type":["string","null"]},"tax_amount":{"type":["string","null"]},"tax_rate":{"type":["string","null"]},"item_code":{"type":["string","null"]},"service_start":{"type":["string","null"]},"service_end":{"type":["string","null"]},"states":{"type":"object","additionalProperties":false,"properties":{"description":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"amount":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"quantity":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"unit":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"unit_price":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"tax_amount":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"tax_rate":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"item_code":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"service_start":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"service_end":{"type":"string","enum":["present","absent","unreadable","conflicting"]}}},"evidence":{"type":"array","maxItems":30,"items":{"type":"object","additionalProperties":false,"required":["field","page","quote"],"properties":{"field":{"type":"string","enum":["description","amount","quantity","unit","unit_price","tax_amount","tax_rate","item_code","service_start","service_end"]},"page":{"type":"integer","minimum":1},"quote":{"type":"string","maxLength":2000}}}}}},"source_resolution":{"type":["string","null"]},"quantity_resolution":{"type":["string","null"]}}}},"coverage_resolution":{"type":["string","null"]}}}}}"#,
    },
    CommandSpec {
        name: "document.review_preview",
        summary: "Validate review fields without saving, reserving or approving anything.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":["document_id","extraction_id","expected_version","arrangement_version","request_id","payload"],"properties":{"document_id":{"type":"string","format":"uuid"},"extraction_id":{"type":["string","null"],"format":"uuid"},"expected_version":{"type":"integer","minimum":0},"arrangement_version":{"type":"integer","minimum":0},"request_id":{"type":"string","format":"uuid"},"payload":{"type":"object","additionalProperties":false,"required":["vendor_id","invoice_number","invoice_date","due_date","total","currency","memo","rows","tax","tax_basis","source_checked"],"properties":{"vendor_id":{"type":["string","null"]},"invoice_number":{"type":"string"},"invoice_date":{"type":["string","null"]},"due_date":{"type":["string","null"]},"total":{"type":"string"},"currency":{"type":"string"},"memo":{"type":"string"},"tax":{"type":"string"},"tax_basis":{"type":"string","enum":["included","additional","unknown"]},"source_checked":{"type":"boolean"},"rows":{"type":"array","maxItems":1000,"items":{"type":"object","additionalProperties":false,"required":["id","description","amount","source_index","excluded_reason"],"properties":{"id":{"type":"string"},"description":{"type":"string"},"amount":{"type":"string"},"source_index":{"type":["integer","null"],"minimum":0},"excluded_reason":{"type":["string","null"]},"details":{"type":["object","null"],"additionalProperties":false,"properties":{"quantity":{"type":["string","null"]},"unit":{"type":["string","null"]},"unit_price":{"type":["string","null"]},"tax_amount":{"type":["string","null"]},"tax_rate":{"type":["string","null"]},"item_code":{"type":["string","null"]},"service_start":{"type":["string","null"]},"service_end":{"type":["string","null"]},"states":{"type":"object","additionalProperties":false,"properties":{"description":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"amount":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"quantity":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"unit":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"unit_price":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"tax_amount":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"tax_rate":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"item_code":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"service_start":{"type":"string","enum":["present","absent","unreadable","conflicting"]},"service_end":{"type":"string","enum":["present","absent","unreadable","conflicting"]}}},"evidence":{"type":"array","maxItems":30,"items":{"type":"object","additionalProperties":false,"required":["field","page","quote"],"properties":{"field":{"type":"string","enum":["description","amount","quantity","unit","unit_price","tax_amount","tax_rate","item_code","service_start","service_end"]},"page":{"type":"integer","minimum":1},"quote":{"type":"string","maxLength":2000}}}}}},"source_resolution":{"type":["string","null"]},"quantity_resolution":{"type":["string","null"]}}}},"coverage_resolution":{"type":["string","null"]}}}}}"#,
    },
    CommandSpec {
        name: "document.review_accept_summary",
        summary: "Accept a summary-only bill from the saved review with a reason. Preserves all rows in history; requires reviewed headers and source. Never approves or releases payment.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":["document_id","expected_version","arrangement_version","request_id","reason"],"properties":{"document_id":{"type":"string","format":"uuid"},"expected_version":{"type":"integer"},"arrangement_version":{"type":"integer"},"request_id":{"type":"string","format":"uuid"},"reason":{"type":"string","minLength":1,"maxLength":1000}}}"#,
    },
    CommandSpec {
        name: "notification.settings",
        summary: "Notification settings and whether a real email sender is configured.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "notification.configure",
        summary: "Enable workflow email notices for currently actionable bills. Approval reminders start after 48 hours, daily thereafter, at most three.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["enabled","reminders"],"properties":{"enabled":{"type":"boolean"},"reminders":{"type":"boolean"}}}"#,
    },
    CommandSpec {
        name: "notification.list",
        summary: "Delivery history for one bill. Sent means accepted by the provider, not confirmed delivery.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "notification.queue",
        summary: "Queue only due notifications for eligible people on the current pinned workflow step. Company opt-in required; deduplicated.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "notification.deliver",
        summary: "Deliver a queued workflow notification after rechecking eligibility. No arbitrary recipient or content. Uncertain delivery is never automatically retried.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["notification_id"],"properties":{"notification_id":{"type":"string","format":"uuid"}}}"#,
    },
    // ── Vendors ─────────────────────────────────────────────────────────────
    CommandSpec {
        name: "vendor.create",
        summary: "Create a vendor.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["name"],"properties":{"name":{"type":"string"},"email":{"type":"string","format":"email"},"terms_days":{"type":"integer","minimum":0,"maximum":365,"description":"Payment terms, e.g. 30 for net 30"}}}"#,
    },
    CommandSpec {
        name: "vendor.update",
        summary: "Change a vendor's name, email or payment terms. Send null to clear email or terms; leave a field out to keep it. Only a person can change name or email, since they decide which invoices match the vendor and where payment-detail invitations go.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["vendor_id"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"name":{"type":"string"},"email":{"type":["string","null"],"format":"email"},"terms_days":{"type":["integer","null"],"minimum":0,"maximum":365,"description":"Payment terms, e.g. 30 for net 30"}}}"#,
    },
    CommandSpec {
        name: "vendor.list",
        summary: "List vendors with their open bill count and amount, whether each has its own workflow (else it follows the organization default), and how many workflow proposals wait for a person.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "vendor.get",
        summary: "A vendor with its account numbers, the workflow its bills follow and any open workflow proposals.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","required":["vendor_id"],"properties":{"vendor_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "vendor.add_account",
        summary: "Record an account number this vendor uses for you, optionally tied to a location (a job site, a store). Invoices quoting it are matched to this vendor and location.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["vendor_id","account_number"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"account_number":{"type":"string"},"location":{"type":"string","description":"e.g. 1420 Elm St site"}}}"#,
    },
    CommandSpec {
        name: "vendor.remove_account",
        summary: "Remove an account number from a vendor, e.g. one recorded by mistake. Invoices quoting it no longer match this vendor by it.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["vendor_id","account_id"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"account_id":{"type":"string","format":"uuid","description":"The account's id from vendor.get"}}}"#,
    },
    CommandSpec {
        name: "vendor.invite_payee",
        summary: "Ask Mercury to email this vendor an invitation to supply payment details. Only a person may send it. Preview sends nothing. Completion proposes a payee; a person must still accept it. Uncertain delivery is never automatically retried.",
        tier: RiskTier::Confirmed,
        person_only: true,
        permission: Permission::Pay,
        input_schema: r#"{"type":"object","required":["vendor_id","email","method"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"email":{"type":"string","format":"email"},"method":{"type":"string","enum":["ach","check","domestic_wire","real_time_payment"]}}}"#,
    },
    CommandSpec {
        name: "vendor.refresh_payee_invite",
        summary: "Check a Mercury invitation. Once completed, propose its recipient for the vendor without accepting it. Payvy also checks open invitations itself every few minutes. If delivery was uncertain, a person can recover it by supplying the invitation ID from Mercury; the email and method must match.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Pay,
        input_schema: r#"{"type":"object","required":["invite_id"],"properties":{"invite_id":{"type":"string","format":"uuid"},"mercury_invite_id":{"type":"string","description":"Only for recovering an invitation whose delivery was uncertain"}}}"#,
    },
    CommandSpec {
        name: "vendor.propose_payee",
        summary: "Propose who this vendor's bills are paid to: a recipient already set up in the connected bank (see bank.recipients) and a method. The account's last four digits are shown for checking. Nothing changes until a person accepts it with vendor.accept_payee. Use this when a vendor's bank details change.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["vendor_id","recipient_id","method"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"recipient_id":{"type":"string","description":"The recipient's id in the bank"},"method":{"type":"string","enum":["ach","check","domestic_wire","real_time_payment"]},"reason":{"type":"string","description":"Why, for the person reviewing it, e.g. where the new details came from"},"connection_id":{"type":"string","format":"uuid"},"instrument_id":{"type":"string","format":"uuid"},"instrument_fingerprint":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "vendor.create_recipient",
        summary: "Create this vendor as a recipient in the connected Mercury account from bank details a person has, and propose it as the vendor's payee (a person still accepts it). Only a person may run it. Preview shows the last four and sends nothing. The numbers go to Mercury and are never stored. Prefer vendor.invite_payee, which keeps the numbers out of Payvy entirely. An unclear answer is never retried: check Mercury's recipients first.",
        tier: RiskTier::Confirmed,
        person_only: true,
        permission: Permission::Pay,
        input_schema: r#"{"type":"object","required":["vendor_id","method","address"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"method":{"type":"string","enum":["ach","check","domestic_wire","real_time_payment"]},"name":{"type":"string","description":"Defaults to the vendor's name"},"email":{"type":"string","format":"email","description":"Defaults to the vendor's email"},"account_number":{"type":"string","description":"Not for check"},"routing_number":{"type":"string","description":"9-digit ABA routing number; not for check"},"account_type":{"type":"string","enum":["business_checking","business_savings","personal_checking","personal_savings"],"description":"ACH only; default business_checking"},"address":{"type":"object","required":["address1","city","region","postal_code"],"description":"The vendor's legal address (the mailing address for a check)","properties":{"address1":{"type":"string"},"address2":{"type":"string"},"city":{"type":"string"},"region":{"type":"string","description":"State, e.g. AZ"},"postal_code":{"type":"string"},"country":{"type":"string","description":"ISO 3166 alpha-2; default US"}}}}}"#,
    },
    CommandSpec {
        name: "vendor.accept_payee",
        summary: "Accept a payee proposal: this vendor's bills are paid to it from now on. Check the destination with the vendor first; changed bank details are the most common way AP fraud happens.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Pay,
        input_schema: r#"{"type":"object","required":["proposal_id"],"properties":{"proposal_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "vendor.decline_payee",
        summary: "Decline a payee proposal.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Pay,
        input_schema: r#"{"type":"object","required":["proposal_id"],"properties":{"proposal_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    // ── Workflows ───────────────────────────────────────────────────────────
    CommandSpec {
        name: "workflow.get",
        summary: "The workflow a vendor's bills follow (or, without vendor_id, the organization default): its steps, a Mermaid chart, where it comes from, and open proposals.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{"vendor_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "workflow.propose",
        summary: "Propose a workflow for a vendor, or without vendor_id for the organization default. Nothing changes until a person accepts it with workflow.accept. Steps run in order and the last is the only pay step. Bills already received keep the workflow they arrived under.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Admin,
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
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["proposal_id"],"properties":{"proposal_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "workflow.decline",
        summary: "Decline a workflow proposal.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["proposal_id"],"properties":{"proposal_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    // ── Bills ───────────────────────────────────────────────────────────────
    CommandSpec {
        name: "bill.create",
        summary: "Enter a bill, optionally from an inbox document, or from some of its pages (pages): those are split into a new document filed with the bill, atomically, and the rest stays in the inbox. It follows the vendor's current workflow from now on. If this vendor already has a bill with the same invoice number (ignoring case, spaces, punctuation and leading zeros), the copy stacks on that bill instead of becoming a second payable one: same amount is a recorded resend, a different amount is a revision that blocks payment until a person accepts or rejects it. allow_duplicate instead makes a separate bill, and that choice is recorded.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["vendor_id","invoice_number","invoice_date","amount_cents"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"invoice_number":{"type":"string"},"invoice_date":{"type":"string","format":"date"},"due_date":{"type":"string","format":"date","description":"Defaults to invoice_date plus the vendor's terms"},"amount_cents":{"type":"integer","minimum":1},"memo":{"type":"string"},"external_ref":{"type":"string","description":"Your own id for this bill (a PO or order number), so other systems can signal it"},"lines":{"type":"array","items":{"type":"object","required":["description","amount_cents"],"properties":{"description":{"type":"string"},"amount_cents":{"type":"integer"},"account":{"type":"string"},"quantity":{"type":"string","description":"As printed, e.g. 2.5; quantity-checked against PO lines when allocated"},"unit":{"type":"string"},"unit_price_cents":{"type":"integer"}}},"description":"If given, they must add up to amount_cents"},"allow_duplicate":{"type":"boolean"},"document_id":{"type":"string","format":"uuid","description":"The inbox document this bill was read from; it is filed with the bill, which pins its current page arrangement"},"review_id":{"type":"string","format":"uuid","description":"Current saved invoice review; required for enrolled source documents. Bill lines are derived server-side."},"extraction_id":{"type":"string","format":"uuid","description":"The document.extract result this bill was entered from (its bill_input carries it). The audit log records which fields were corrected."},"pages":{"type":"array","minItems":1,"items":{"type":"integer","minimum":1},"description":"Only these pages of document_id (page numbers of the original, in this order) are this bill: they are split into a new document filed with the bill, in the same change, and the rest stays in the inbox. Naming every visible page files the whole document."},"expected_version":{"type":"integer","minimum":0,"description":"The document's pages.version you saw; refused if the pages changed since"}}}"#,
    },
    CommandSpec {
        name: "bill.list",
        summary: "List bills, newest first. Filter by state (open, rejected, void, paid), by where open bills stand (status: awaiting_approval, held, waiting_for, awaiting_release, ready_to_pay, released), by vendor, by invoice_number (compared the way duplicates are: ignoring case, spaces, punctuation and leading zeros), or to bills waiting on you (mine). Each bill says received_count (how many copies of its invoice arrived) and pending_revision_amount_cents when a revised copy awaits a decision.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{"state":{"type":"string","enum":["open","rejected","void","paid"]},"status":{"type":"string","enum":["awaiting_approval","held","waiting_for","awaiting_release","ready_to_pay","released"]},"vendor_id":{"type":"string","format":"uuid"},"invoice_number":{"type":"string","description":"Bills with this invoice number, compared the way duplicates are"},"mine":{"type":"boolean","description":"Only bills a step is waiting on you for"},"limit":{"type":"integer","minimum":1,"maximum":500}}}"#,
    },
    CommandSpec {
        name: "bill.get",
        summary: "A bill with its lines, its workflow progress step by step, approvals, signals and history, every copy of the invoice received for it (arrivals, oldest first, with received_count and any pending_revision awaiting a person's decision), and the page arrangement of its document it pinned. While it waits for approval, approval says whether you can give it and, if not, why and who else can.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bill.revision_accept",
        summary: "Accept a revised invoice stacked on this bill (person only): the latest copy's amount and lines become the payable ones, and the bill can be paid again. The prior version stays in the bill's stack, not payable. This prepares the bill; it is not approval.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bill.revision_reject",
        summary: "Reject a revised invoice stacked on this bill (person only): the bill keeps the amount it already carries, the revised copy stays in its stack, and the bill can be paid again.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bill.approve",
        summary: "Approve a bill at its current approval step. You must be one of that step's approvers. One person gives a bill one approval, so you can't approve a second step, or a step a later one needs you for; and when approval_rules has submitter_cannot_approve on, whoever entered the bill or uploaded its document can't approve it. A refusal says who else can. bill.get shows it in approval before you try.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Approve,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"},"note":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "bill.reject",
        summary: "Reject a bill: it will not be paid. Final.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Approve,
        input_schema: r#"{"type":"object","required":["bill_id","reason"],"properties":{"bill_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "bill.signal",
        summary: "Report that something outside Payvy happened for a bill, e.g. customer_funded when the customer has paid for this cost. A workflow's wait_for step waits for its signal. Name the bill by bill_id or by the external_ref it was created with.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["signal"],"properties":{"bill_id":{"type":"string","format":"uuid"},"external_ref":{"type":"string"},"signal":{"type":"string","description":"a-z 0-9 _, e.g. customer_funded"},"note":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "bill.release",
        summary: "Release a bill for payment at its pay step. With a bank connected, this sends the payment through it to the vendor's accepted payee; the preview shows exactly what will be sent. Preview first; commit with the returned token.",
        tier: RiskTier::Confirmed,
        person_only: true,
        permission: Permission::Pay,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"},"pay_elsewhere":{"type":"boolean","description":"Release without paying through the connected bank; record the payment later with bill.mark_paid"}}}"#,
    },
    CommandSpec {
        name: "bill.auto_pay",
        summary: "Pay a bill whose workflow pays automatically (status ready_to_pay) through the connected Mercury account, to the vendor's accepted payee. Payvy does this on its own within a minute of the bill getting there; run it to pay one now. A bill over the auto-pay limit (auto_pay.limits) is never paid this way; it waits for a person to release it. If the payment doesn't go, the bill waits for a person to release it and is never retried automatically.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Pay,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bill.mark_paid",
        summary: "Record that a released or ready bill was paid outside Payvy (a check, a payment made in the bank's own app). Payments sent through the connected bank are recorded on their own. Preview first; commit with the returned token.",
        tier: RiskTier::Confirmed,
        person_only: false,
        permission: Permission::Pay,
        input_schema: r#"{"type":"object","required":["bill_id","paid_on","method"],"properties":{"bill_id":{"type":"string","format":"uuid"},"paid_on":{"type":"string","format":"date"},"method":{"type":"string","enum":["ach","check","wire","card","other"]},"reference":{"type":"string","description":"Check number, transfer id"}}}"#,
    },
    CommandSpec {
        name: "bill.void",
        summary: "Void a bill entered by mistake. It no longer counts for duplicate detection. Preview first; commit with the returned token.",
        tier: RiskTier::Confirmed,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["bill_id","reason"],"properties":{"bill_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    // ── Auto-pay limits ─────────────────────────────────────────────────────
    CommandSpec {
        name: "auto_pay.limits",
        summary: "The ceiling on automatic payments: the company's limit (the $5,000.00 default until a person sets one) and each vendor's own limit, which overrides the company's in either direction. A bill over the limit in force (the vendor's if set, else the company's) never pays automatically: it waits for a person to release it, whatever its workflow says.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "auto_pay.lower_limit",
        summary: "Lower the company's auto-pay limit, or with vendor_id set, lower or remove one vendor's own limit, as long as the limit in force for it doesn't go up (removing an override is allowed here when the company's limit is no higher). Lowering the company's limit doesn't change vendors with their own. Bills over the new limit wait for a person to release them. Only a person may change a limit. Anything that raises the limit in force needs auto_pay.raise_limit.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["limit_cents"],"properties":{"vendor_id":{"type":"string","format":"uuid","description":"Leave out for the company's limit"},"limit_cents":{"type":["integer","null"],"minimum":0,"maximum":100000000000,"description":"null (with vendor_id only) removes the vendor's own limit"},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "auto_pay.raise_limit",
        summary: "Raise the company's auto-pay limit (vendors with their own limit keep it), or with vendor_id set a vendor's own limit above what applies to it now (it may be above the company's), or remove a vendor's own limit when the company's is higher (limit_cents null). Only a person may change a limit. The preview lists the bills that would start paying automatically; commit with the returned token.",
        tier: RiskTier::Confirmed,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["limit_cents"],"properties":{"vendor_id":{"type":"string","format":"uuid","description":"Leave out for the company's limit"},"limit_cents":{"type":["integer","null"],"minimum":0,"maximum":100000000000,"description":"null (with vendor_id only) removes the vendor's own limit"},"reason":{"type":"string"}}}"#,
    },
    // ── Purchase orders ─────────────────────────────────────────────────
    CommandSpec {
        name: "po.settings",
        summary: "Whether purchase orders are on for this company. Off (the default) refuses every po.* write and never blocks a payment; reads still work.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "po.configure",
        summary: "Turn purchase orders on or off for the company (person only). When on, each vendor's settings decide whether its bills require a PO, applying at once to bills in flight. Turning them off lifts the payment gate; allocations stay on record.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["enabled"],"properties":{"enabled":{"type":"boolean"}}}"#,
    },
    CommandSpec {
        name: "po.create",
        summary: "Create a draft purchase order for a vendor. Nothing matches against it until a person activates it with po.activate.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["vendor_id","po_number"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"po_number":{"type":"string"},"owner":{"type":"string"},"authorized_cents":{"type":"integer","minimum":1},"service_address":{"type":"string"},"period_start":{"type":"string","format":"date"},"period_end":{"type":"string","format":"date"},"external_source":{"type":"string"},"external_id":{"type":"string"},"lines":{"type":"array","items":{"type":"object","required":["description","amount_cents"],"properties":{"description":{"type":"string"},"quantity":{"type":"number"},"unit":{"type":"string"},"unit_price_cents":{"type":"integer"},"amount_cents":{"type":"integer","minimum":1},"account":{"type":"string"}}}}}}"#,
    },
    CommandSpec {
        name: "po.update",
        summary: "Change a purchase order (person only). Editing a draft sets it up; editing an active PO creates a new revision and never releases existing allocations. Vendor, number and currency cannot change.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["po_id"],"properties":{"po_id":{"type":"string","format":"uuid"},"owner":{"type":["string","null"]},"authorized_cents":{"type":"integer","minimum":1},"reason":{"type":"string"},"service_address":{"type":["string","null"]},"period_start":{"type":["string","null"],"format":"date"},"period_end":{"type":["string","null"],"format":"date"},"lines":{"type":"array","items":{"type":"object","required":["description","amount_cents"],"properties":{"description":{"type":"string"},"quantity":{"type":"number"},"unit":{"type":"string"},"unit_price_cents":{"type":"integer"},"amount_cents":{"type":"integer","minimum":1},"account":{"type":"string"}}}}}}"#,
    },
    CommandSpec {
        name: "po.activate",
        summary: "Activate a draft purchase order (person only). Requires an owner and an authorized amount. Imported POs arrive as drafts and are activated this way; an import never activates.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["po_id"],"properties":{"po_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "po.close",
        summary: "Close an active purchase order (person only), with a reason: fulfilled, expired, settled outside Payvy, or superseded. Allocations already made stand and nothing is released implicitly; their bills wait (ineligible_po) until the allocation is reversed or permitted.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["po_id","reason"],"properties":{"po_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "po.cancel",
        summary: "Cancel a draft or active purchase order (person only), with a reason. Allocations already made stand and nothing is released implicitly; their bills wait (ineligible_po) until the allocation is reversed or permitted.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["po_id","reason"],"properties":{"po_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "po.suspend",
        summary: "Suspend an active purchase order (person only), with a reason: allocations already made stand, new ones are refused, and its bills wait (suspended_po) until po.reactivate or a scoped po.override.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["po_id","reason"],"properties":{"po_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "po.reactivate",
        summary: "Reactivate a suspended purchase order (person only): it takes allocations again and its bills stop waiting on the suspension.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["po_id"],"properties":{"po_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "po.get",
        summary: "A purchase order: lifecycle, billing progress, authorized/reserved/consumed/available, its current lines, revisions and allocation history.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","required":["po_id"],"properties":{"po_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "po.list",
        summary: "Purchase orders, optionally for one vendor, each with authorized/reserved/consumed/available (none on a closed or canceled PO) and billing progress.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{"vendor_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "po.import",
        summary: "Import a purchase order from another system by external source/id (idempotent). Always lands as a draft: a person activates it with po.activate. opening_consumed_cents records amounts already billed in the source system and is not counted twice on a re-import.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["vendor_id","po_number","external_source","external_id"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"po_number":{"type":"string"},"owner":{"type":"string"},"authorized_cents":{"type":"integer","minimum":1},"opening_consumed_cents":{"type":"integer","minimum":0},"source_allocation_id":{"type":"string"},"service_address":{"type":"string"},"period_start":{"type":"string","format":"date"},"period_end":{"type":"string","format":"date"},"lines":{"type":"array","items":{"type":"object","required":["description","amount_cents"],"properties":{"description":{"type":"string"},"quantity":{"type":"number"},"unit":{"type":"string"},"unit_price_cents":{"type":"integer"},"amount_cents":{"type":"integer","minimum":1},"account":{"type":"string"}}}}}}"#,
    },
    CommandSpec {
        name: "po.vendor_settings",
        summary: "A vendor's purchase-order settings: whether its bills require a PO, and its overage caps (dollar and/or percentage in basis points). Caps are pinned onto each PO revision when it is made.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","required":["vendor_id"],"properties":{"vendor_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "po.set_vendor_settings",
        summary: "Set a vendor's purchase-order settings (person only): whether its bills require a PO, and overage caps. Zero overage is the default; an absent cap is not configured, a cap of 0 is zero. Existing PO revisions keep the allowance pinned on them.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["vendor_id","require_po"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"require_po":{"type":"boolean"},"require_funding":{"type":"boolean","description":"Explicitly enabled: an unsatisfied customer_funded signal blocks release and auto-pay"}, "overage_cents_cap":{"type":["integer","null"],"minimum":0},"overage_percent_bp":{"type":["integer","null"],"minimum":0,"maximum":10000}}}"#,
    },
    CommandSpec {
        name: "po.policy_get",
        summary: "A vendor's PO policies in force (the enumerated registry): over_period (flag_only or require_full_cover), quantity_matching (flag_only or enforce), and the version. Version 0 is the defaults: flag; do not silently match.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","required":["vendor_id"],"properties":{"vendor_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "po.policy_set",
        summary: "Set a vendor's PO policies (person only): a new immutable version supersedes the old; bills already created keep the version they pinned. Asking for what is already in force is refused. Agents may not set policy.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["vendor_id"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"over_period":{"type":"string","enum":["flag_only","require_full_cover"]},"quantity_matching":{"type":"string","enum":["flag_only","enforce"],"description":"enforce: quantity_over and rate_mismatch block payment"},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "po.match_preview",
        summary: "Which of a bill's vendor's active POs it could go against: available amounts, a proposed allocation and any exception (no PO, over available). Writes and reserves nothing.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"},"po_number":{"type":"string","description":"A PO reference read from the invoice, verified against the vendor's POs"}}}"#,
    },
    CommandSpec {
        name: "po.match_apply",
        summary: "Reserve bill amounts against purchase orders. Atomic across every PO in the call: all affected POs are locked in a consistent order, the whole set is validated (active, vendor, available plus the revision's pinned allowance, total not over the bill), then everything reserves together or nothing does. Reserving is preparation, not approval.",
        tier: RiskTier::Confirmed,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["bill_id","allocations"],"properties":{"bill_id":{"type":"string","format":"uuid"},"allocations":{"type":"array","minItems":1,"items":{"type":"object","required":["po_id","amount_cents"],"properties":{"po_id":{"type":"string","format":"uuid"},"amount_cents":{"type":"integer","minimum":1}}}}}}"#,
    },
    CommandSpec {
        name: "po.override",
        summary: "Permit one bill's PO exception (person only, separate from vendor approval): bound to this bill, its current POs and allocations, the exception and an authorized excess amount, with a required reason. Relevant changes void it and need a new override. It does not create PO capacity or hide the excess.",
        tier: RiskTier::Confirmed,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["bill_id","authorized_excess_cents","reason"],"properties":{"bill_id":{"type":"string","format":"uuid"},"authorized_excess_cents":{"type":"integer","minimum":0},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "po.allocation_accept",
        summary: "Accept a reserved allocation as part of preparing a bill: the amount moves from reserved to consumed. This is not bill approval.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["allocation_id"],"properties":{"allocation_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "po.allocation_reverse",
        summary: "Release a reserved or consumed allocation with a reason (match rejected, bill rejected, bill voided). Capacity returns by this reversal record; allocations are never deleted. A PO revision never releases an allocation.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["allocation_id","reason"],"properties":{"allocation_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    // ── Inbox ───────────────────────────────────────────────────────────────
    CommandSpec {
        name: "inbox.get",
        summary: "What is waiting on the caller: bills a step needs them to approve or release (action approve or release), and payee changes and workflow proposals a person must accept or decline. Agents see it too, to remind people.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    // ── Approval rules ──────────────────────────────────────────────────────
    CommandSpec {
        name: "approval_rules.get",
        summary: "The company's approval rules. submitter_cannot_approve: whoever entered a bill or uploaded its document can't approve it (bills an agent entered are unaffected). One approval per person per bill always applies.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "approval_rules.set",
        summary: "Turn submitter_cannot_approve on or off for the whole company. It applies to approvals from now on, including bills already waiting.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["submitter_cannot_approve"],"properties":{"submitter_cannot_approve":{"type":"boolean"},"reason":{"type":"string"}}}"#,
    },
    // ── People ──────────────────────────────────────────────────────────────
    CommandSpec {
        name: "person.list",
        summary: "Everyone who can sign in to the web app, with the actor name their actions are recorded under.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "person.add",
        summary: "Let someone sign in to the web app with a link sent to their email. They act as the person named by actor, with every right a person has: approving, releasing payments, accepting payees and workflows.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["email","name"],"properties":{"roles":{"type":"array","minItems":1,"maxItems":4,"uniqueItems":true,"items":{"type":"string","enum":["admin","ap_clerk","approver","payment_releaser"]},"description":"Defaults to AP clerk. Admin includes all permissions."},"email":{"type":"string","format":"email"},"name":{"type":"string","description":"Their name, for people"},"actor":{"type":"string","description":"The name their actions are recorded under, a-z 0-9 . _ -; defaults to the part of the email before @"}}}"#,
    },
    CommandSpec {
        name: "person.set_roles",
        summary: "Set a person's company roles. Admin only; roles combine. Cannot remove the last sign-in admin. Changes take effect on their next command, including an existing session or person API key. Audit records old and new roles.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["actor","roles"],"properties":{"actor":{"type":"string"},"roles":{"type":"array","minItems":1,"maxItems":4,"uniqueItems":true,"items":{"type":"string","enum":["admin","ap_clerk","approver","payment_releaser"]}}}}"#,
    },
    CommandSpec {
        name: "person.remove",
        summary: "Stop someone signing in, and sign them out everywhere. Their past actions stay on the record. Refused for yourself (someone else removes you) and for the last person who can sign in.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["actor"],"properties":{"actor":{"type":"string"}}}"#,
    },
    // ── Organization ────────────────────────────────────────────────────────
    CommandSpec {
        name: "org.get",
        summary: "This organization: its id, its name as people see it in the app and sign-in emails, and when it was created.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "org.update",
        summary: "Rename the organization. The name is trimmed, 1 to 100 characters, and must differ from the current one. Bills, vendors and events are unchanged.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["name"],"properties":{"name":{"type":"string","minLength":1,"maxLength":100}}}"#,
    },
    // ── Bank and payments ───────────────────────────────────────────────────
    CommandSpec {
        name: "bank.authorize",
        summary: "Begin Ramp OAuth with read scopes. Returns a consent URL; the current bank remains active. Authorization expires after ten minutes.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":[],"properties":{}}"#,
    },
    CommandSpec {
        name: "bank.complete_authorization",
        summary: "Complete Ramp OAuth for the same person and company. Seals credentials but does not activate a connection. Never retry an unclear code exchange; start authorization again.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["state","code"],"properties":{"state":{"type":"string"},"code":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "bank.authorization",
        summary: "Read fresh Ramp entity and funding choices for your staged authorization. Choose an exact account and fingerprint before activation.",
        tier: RiskTier::Read,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["authorization_id"],"properties":{"authorization_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bank.activate",
        summary: "Select a staged Ramp connection for setup and payee mapping, explicitly replacing the current paying bank. Preview and confirm the exact account fingerprint and previous connection ID (null if none). Ramp sending and auto-pay remain disabled.",
        tier: RiskTier::Confirmed,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["authorization_id","entity_id","account_id","fingerprint","replacing_connection_id"],"properties":{"authorization_id":{"type":"string","format":"uuid"},"entity_id":{"type":"string","format":"uuid"},"account_id":{"type":"string","format":"uuid"},"fingerprint":{"type":"string"},"replacing_connection_id":{"type":["string","null"],"format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bank.maintain",
        summary: "Check or renew an existing Ramp grant when due. The background worker uses this command. It cannot authorize or activate connections, accept payees or send payments. Failures persist a bounded retry schedule or require reconnection. Preview sends nothing.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["connection_id"],"properties":{"connection_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bank.renew",
        summary: "Check a live Ramp connection and renew access when needed. A person can resume checks paused after repeated temporary failures, once cooldown permits. Rejected grants require reconnecting. Preview sends nothing.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["connection_id"],"properties":{"connection_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bank.connect",
        summary: "Connect a bank with an API token from its dashboard. Mercury pays bills: mode request (default) waits in Mercury for a Mercury admin (Custom token, RequestSendMoney scope); mode direct sends at once (Read and Write token, server IP allowlisted). Rho is always read-only (no mode to choose): it never pays and never replaces the paying bank; create its token in Rho Settings > API > Access Tokens with accounts:read and transactions:read. Payvy keeps tokens encrypted and never holds money or account numbers.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","required":["provider","token"],"properties":{"provider":{"type":"string","enum":["mercury","rho"]},"token":{"type":"string"},"environment":{"type":"string","enum":["production","sandbox"]},"mode":{"type":"string","enum":["request","direct"],"description":"Mercury only: how payments are sent. Rho is always read-only."},"account_id":{"type":"string","description":"The account to pay from (Mercury) or display (Rho), if there is more than one"}}}"#,
    },
    CommandSpec {
        name: "bank.get",
        summary: "The selected bank and all live connections, with capabilities (pay, read_only, setup). Ramp includes connection health, retry time and recovery actions; setup never enables payments.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "bank.disconnect",
        summary: "Disconnect a bank (the paying one by default, or provider rho for the read-only one). No new payments are sent through a disconnected paying bank; ones already sent are still tracked.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","properties":{"provider":{"type":"string","enum":["mercury","rho","ramp"]}}}"#,
    },
    CommandSpec {
        name: "bank.accounts",
        summary: "Accounts at the read-only Rho connection, with balances in cents. Empty object input; refuses when no Rho connection is live.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "bank.transactions",
        summary: "Transactions from the read-only Rho connection: signed amounts in cents (negative is money out), counterparty, status and posted time. Filter by account, date and search text.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{"account_id":{"type":"string"},"posted_after":{"type":"string","description":"ISO 8601, inclusive"},"posted_before":{"type":"string","description":"ISO 8601, exclusive"},"initiated_after":{"type":"string"},"initiated_before":{"type":"string"},"search":{"type":"string","description":"Counterparty, memo or note text"},"limit":{"type":"integer","minimum":1,"maximum":1000}}}"#,
    },
    CommandSpec {
        name: "bank.recipients",
        summary: "Discover existing bank payees. Ramp lists vendors first; pass recipient_id to list specific ACH instruments, masked destinations and fingerprints for vendor.propose_payee. Never guess an instrument or enter bank numbers here.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{"search":{"type":"string","description":"Part of the name"},"recipient_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "payment.list",
        summary: "Payments sent through the bank, newest first, or only those still waiting on the bank.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{"open":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":500}}}"#,
    },
    CommandSpec {
        name: "payment.refresh",
        summary: "Ask the bank where a payment stands now and record it: sent marks the bill paid; failed, cancelled or rejected sends the bill back for release; returned reopens a paid bill. Payvy does this on its own every minute or two.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Pay,
        input_schema: r#"{"type":"object","required":["payment_id"],"properties":{"payment_id":{"type":"string","format":"uuid"}}}"#,
    },
    // ── Documents ───────────────────────────────────────────────────────────
    CommandSpec {
        name: "document.upload",
        summary: "Add a file to the inbox: a PDF or image (PNG, JPEG, GIF, WebP, TIFF) of a bill, up to 25 MB, as base64. Its type is read from the file itself. Refused if the same file is already here, naming the existing document.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["filename","content_base64"],"properties":{"filename":{"type":"string"},"content_base64":{"type":"string","description":"The file, base64-encoded (standard alphabet)"}}}"#,
    },
    CommandSpec {
        name: "document.list",
        summary: "Documents in the inbox (default), oldest first; or filed or trashed ones, or split ones (every page moved to other documents). A document split from another has split_from: its id and filename. Each says what its latest reading found (reading: vendor and how it matched, invoice_count, invoice_numbers, total_cents, dates, failed_checks, ready; null until read) and its cover: the first page shown, for a thumbnail.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{"status":{"type":"string","enum":["inbox","filed","trashed","split"]},"limit":{"type":"integer","minimum":1,"maximum":500}}}"#,
    },
    CommandSpec {
        name: "document.get",
        summary: "A document's details, every time a model read it (newest first, with checks, the suggested bill_input, and out_of_date when its pages were rearranged since), its pages (page count of the original file, the current arrangement and version, every earlier version, and whether they are locked and why), split_from (the document it was split from) and split_into (documents split from it, with their pages), its history, and a download link to the original file valid for 15 minutes. A reading of a file with several invoices has invoices: the pages, invoice number and total of each.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","required":["document_id"],"properties":{"document_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "document.extract",
        summary: "Read an inbox document with the configured model: vendor, invoice number, dates, lines and total, checked against each other and matched to a vendor on file. Returns the checks, whether it is ready, and bill_input: the bill.create input it suggests, to enter as is or corrected. Nothing becomes a bill until bill.create. New documents are read automatically when the server's reader is on. Refused if the document was already read, unless again is true. The model gets the original file and, when the document is only some of its pages (split from it, or with pages hidden), is told which pages to read. When it finds several invoices in the file, the reading lists them (invoices) and fails the one_invoice check; an agent's reading then splits the file straight away into one document per invoice (split.to_read: read each next), which document.unsplit undoes. A person's reading only suggests. A document already split from another is never split again this way.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["document_id"],"properties":{"document_id":{"type":"string","format":"uuid"},"again":{"type":"boolean","description":"Read it again even though it was already read, e.g. after adding the vendor"}}}"#,
    },
    CommandSpec {
        name: "document.arrange",
        summary: "Set a document's page order, rotation and hidden pages in one change, as a new version of its arrangement; the original file never changes. List every page of this document once, by its page number in the original file, in the order to show them; at least one must stay visible. expected_version is the version from document.get: if someone changed the pages since, it is refused and you get the document again. Refused for a document set aside as not a bill, and once its bill has an approval or is no longer open (pages lock after approval). A bill still waiting for its first approval moves to the new version, and its history says so.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["document_id","expected_version","pages"],"properties":{"document_id":{"type":"string","format":"uuid"},"expected_version":{"type":"integer","minimum":0,"description":"pages.version from document.get; 0 while the pages were never arranged"},"pages":{"type":"array","minItems":1,"description":"Every page of this document (pages.arrangement), once each, in the order to show them","items":{"type":"object","required":["page"],"additionalProperties":false,"properties":{"page":{"type":"integer","minimum":1,"description":"The page number in the original file"},"rotation":{"type":"integer","enum":[0,90,180,270],"description":"Degrees clockwise; default 0"},"hidden":{"type":"boolean","description":"Left out of the view; it can be restored. Default false"}}}}}}"#,
    },
    CommandSpec {
        name: "document.split",
        summary: "Move groups of pages into new documents, one per group, for a file holding several invoices. Each new document is a view of the same original file (nothing is copied), shows only its pages in the order given with their rotation, records which document it came from, and is in the inbox to be read and entered. The moved pages leave this document's next arrangement version; if none are left it becomes split and leaves the inbox. expected_version is pages.version from document.get. Refused for a document set aside or locked, for a stale version, for groups that overlap, name pages this document doesn't have, or would leave a new document with nothing to show. Takes effect at once; document.unsplit undoes it.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["document_id","expected_version","groups"],"properties":{"document_id":{"type":"string","format":"uuid"},"expected_version":{"type":"integer","minimum":0,"description":"pages.version from document.get"},"groups":{"type":"array","minItems":1,"description":"One list per new document: page numbers of the original, in the order to show them","items":{"type":"array","minItems":1,"items":{"type":"integer","minimum":1}}}}}"#,
    },
    CommandSpec {
        name: "document.create_bills",
        summary: "Make a bill of each invoice in a file of several, in one request: the file is split at once (one new document per group of pages, as document.split does), and the server then reads each part and creates its bill in the background, as the person who asked. A part that reads cleanly becomes a bill; a copy of an invoice already here stacks on that bill (a resend, or a revision when the amount differs); any other part waits in To review for a person, with the reason. Pages in leftover are part of no bill and stay with this document. groups and leftover together must name every visible page once. request_id makes it safe to retry: the same request_id returns the batch already made instead of splitting again. document.get on this document shows the batch's progress (bill_batch).",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["document_id","expected_version","request_id","groups"],"properties":{"document_id":{"type":"string","format":"uuid"},"expected_version":{"type":"integer","minimum":0,"description":"pages.version from document.get"},"request_id":{"type":"string","format":"uuid","description":"A new id per request; a retry with the same id returns the same batch"},"groups":{"type":"array","minItems":1,"maxItems":200,"description":"One list per invoice, in order: page numbers of the original, in the order to show them","items":{"type":"array","minItems":1,"items":{"type":"integer","minimum":1}}},"leftover":{"type":"array","description":"Visible pages that are part of no bill; they stay with this document","items":{"type":"integer","minimum":1}}}}"#,
    },
    CommandSpec {
        name: "document.unsplit",
        summary: "Undo a split: a document split from another gives its pages back to it (in their original place where it can tell), and the other document comes back to the inbox if it had no pages left. The split document becomes split and leaves the inbox. Refused once the split document is filed with a bill or set aside, or when the document it came from is locked.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["document_id"],"properties":{"document_id":{"type":"string","format":"uuid","description":"The document that was split off"}}}"#,
    },
    CommandSpec {
        name: "document.trash",
        summary: "Take a document out of the inbox because it isn't a bill (a statement, a marketing email, a duplicate). The file is kept.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["document_id","reason"],"properties":{"document_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    // ── Email ───────────────────────────────────────────────────────────────
    CommandSpec {
        name: "email.address",
        summary: "This organization's inbound address. Bills emailed or forwarded to it land in the document inbox; a +tag after the token is allowed (token+fuel@...). Null until a person creates one with email.new_address.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "email.new_address",
        summary: "Create the inbound address, or replace it if it is getting spam. The old address stops working at once, so anyone forwarding to it must switch.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "email.import",
        summary: "Read a raw email (an .eml file, base64) into the inbox: its PDF and image attachments become documents, including those of a forwarded email. The inbound webhook uses this too. Refused if the same email is already here. The result lists what was skipped and why.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","required":["raw_base64"],"properties":{"raw_base64":{"type":"string","description":"The whole RFC 5322 message, base64-encoded (standard alphabet)"},"recipient":{"type":"string","description":"The address it was delivered to, if known"}}}"#,
    },
    CommandSpec {
        name: "email.list",
        summary: "Emails received, newest first: sender, subject and what was skipped.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":500}}}"#,
    },
    CommandSpec {
        name: "email.get",
        summary: "One email: its details, the documents it brought in, and a 15-minute download link for the original message.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","required":["email_id"],"properties":{"email_id":{"type":"string","format":"uuid"}}}"#,
    },
    // ── Ledger ──────────────────────────────────────────────────────────────
    CommandSpec {
        name: "ledger.connect",
        summary: "Connect VisiBooks for this company (person only): its HTTPS origin, entity id and API key. The key is sealed at once and never read back; reconnecting updates the same connection, so its links stand. Confirmed, because it carries a credential.",
        tier: RiskTier::Confirmed,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":["origin","entity","api_key"],"properties":{"origin":{"type":"string"},"entity":{"type":"string"},"api_key":{"type":"string"},"post_when":{"type":"string","enum":["approval","payment"]}}}"#,
    },
    CommandSpec {
        name: "ledger.settings",
        summary: "The company's ledger connection: provider, origin, entity, when it posts (post_when), the AP account and the bank account map. Never the credential. Disconnected companies read connected: false.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
        input_schema: r#"{"type":"object","additionalProperties":false,"properties":{}}"#,
    },
    CommandSpec {
        name: "ledger.configure",
        summary: "Change how the connected ledger posts (person only): post_when (approval or payment), the AP account, coding defaults, and the map from Payvy bank connections (or payment methods) to ledger bank accounts. Asking for what is already set is refused.",
        tier: RiskTier::Routine,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","additionalProperties":false,"properties":{"post_when":{"type":"string","enum":["approval","payment"]},"ap_account_id":{"type":["string","null"]},"defaults":{"type":"object"},"bank_account_map":{"type":"object"}}}"#,
    },
    CommandSpec {
        name: "ledger.post_bill",
        summary: "Post one bill to the connected ledger. The same bill posted again returns the first ledger id and sends nothing. A changed bill after posting is a conflict. A rejected or void bill is refused. Preview sends nothing.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "ledger.post_payment",
        summary: "Record one sent payment in the ledger, applied to its posted bill and drawn on the mapped ledger bank account. Payvy has already paid through the customer's bank. Preview sends nothing.",
        tier: RiskTier::Routine,
        person_only: false,
        permission: Permission::Prepare,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":["payment_id"],"properties":{"payment_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "ledger.void",
        summary: "Void the ledger's copy of a bill. Every posted payment is voided first, oldest first, including one the bank later returned. If a payment void is refused the bill stays posted. This cannot be undone, so it asks for confirmation. Only a person with admin may do this. Preview sends nothing.",
        tier: RiskTier::Confirmed,
        person_only: true,
        permission: Permission::Admin,
        input_schema: r#"{"type":"object","additionalProperties":false,"required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    // ── Audit ───────────────────────────────────────────────────────────────
    CommandSpec {
        name: "event.list",
        summary: "The audit log, newest first: every change, who made it and when. Filter to one bill, vendor or proposal with entity_id.",
        tier: RiskTier::Read,
        person_only: false,
        permission: Permission::Read,
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
    fn shared_reads_are_available_to_agents() {
        for c in COMMANDS.iter().filter(|c| c.tier == RiskTier::Read) {
            // A pending OAuth grant belongs to the initiating person, not an
            // organization-wide connection. Its setup discovery is private.
            assert_eq!(c.person_only, c.name == "bank.authorization", "{}", c.name);
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
