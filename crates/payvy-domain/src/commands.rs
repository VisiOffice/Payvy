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
        summary: "List vendors with their open bill count and amount, whether each has its own workflow (else it follows the organization default), and how many workflow proposals wait for a person.",
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
    CommandSpec {
        name: "vendor.invite_payee",
        summary: "Ask Mercury to email this vendor an invitation to supply payment details. Only a person may send it. Preview sends nothing. Completion proposes a payee; a person must still accept it. Uncertain delivery is never automatically retried.",
        tier: RiskTier::Confirmed,
        person_only: true,
        input_schema: r#"{"type":"object","required":["vendor_id","email","method"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"email":{"type":"string","format":"email"},"method":{"type":"string","enum":["ach","check","domestic_wire","real_time_payment"]}}}"#,
    },
    CommandSpec {
        name: "vendor.refresh_payee_invite",
        summary: "Check a Mercury invitation. Once completed, propose its recipient for the vendor without accepting it. Payvy also checks open invitations itself every few minutes. If delivery was uncertain, a person can recover it by supplying the invitation ID from Mercury; the email and method must match.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["invite_id"],"properties":{"invite_id":{"type":"string","format":"uuid"},"mercury_invite_id":{"type":"string","description":"Only for recovering an invitation whose delivery was uncertain"}}}"#,
    },
    CommandSpec {
        name: "vendor.propose_payee",
        summary: "Propose who this vendor's bills are paid to: a recipient already set up in the connected bank (see bank.recipients) and a method. The account's last four digits are shown for checking. Nothing changes until a person accepts it with vendor.accept_payee. Use this when a vendor's bank details change.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["vendor_id","recipient_id","method"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"recipient_id":{"type":"string","description":"The recipient's id in the bank"},"method":{"type":"string","enum":["ach","check","domestic_wire","real_time_payment"]},"reason":{"type":"string","description":"Why, for the person reviewing it, e.g. where the new details came from"}}}"#,
    },
    CommandSpec {
        name: "vendor.create_recipient",
        summary: "Create this vendor as a recipient in the connected Mercury account from bank details a person has, and propose it as the vendor's payee (a person still accepts it). Only a person may run it. Preview shows the last four and sends nothing. The numbers go to Mercury and are never stored. Prefer vendor.invite_payee, which keeps the numbers out of Payvy entirely. An unclear answer is never retried: check Mercury's recipients first.",
        tier: RiskTier::Confirmed,
        person_only: true,
        input_schema: r#"{"type":"object","required":["vendor_id","method","address"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"method":{"type":"string","enum":["ach","check","domestic_wire","real_time_payment"]},"name":{"type":"string","description":"Defaults to the vendor's name"},"email":{"type":"string","format":"email","description":"Defaults to the vendor's email"},"account_number":{"type":"string","description":"Not for check"},"routing_number":{"type":"string","description":"9-digit ABA routing number; not for check"},"account_type":{"type":"string","enum":["business_checking","business_savings","personal_checking","personal_savings"],"description":"ACH only; default business_checking"},"address":{"type":"object","required":["address1","city","region","postal_code"],"description":"The vendor's legal address (the mailing address for a check)","properties":{"address1":{"type":"string"},"address2":{"type":"string"},"city":{"type":"string"},"region":{"type":"string","description":"State, e.g. AZ"},"postal_code":{"type":"string"},"country":{"type":"string","description":"ISO 3166 alpha-2; default US"}}}}}"#,
    },
    CommandSpec {
        name: "vendor.accept_payee",
        summary: "Accept a payee proposal: this vendor's bills are paid to it from now on. Check the destination with the vendor first; changed bank details are the most common way AP fraud happens.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","required":["proposal_id"],"properties":{"proposal_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    CommandSpec {
        name: "vendor.decline_payee",
        summary: "Decline a payee proposal.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","required":["proposal_id"],"properties":{"proposal_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
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
        summary: "Enter a bill, optionally from an inbox document. It follows the vendor's current workflow from now on. Refused if this vendor already has a bill with the same invoice number (ignoring case, spaces, punctuation and leading zeros) unless allow_duplicate is true.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["vendor_id","invoice_number","invoice_date","amount_cents"],"properties":{"vendor_id":{"type":"string","format":"uuid"},"invoice_number":{"type":"string"},"invoice_date":{"type":"string","format":"date"},"due_date":{"type":"string","format":"date","description":"Defaults to invoice_date plus the vendor's terms"},"amount_cents":{"type":"integer","minimum":1},"memo":{"type":"string"},"external_ref":{"type":"string","description":"Your own id for this bill (a PO or order number), so other systems can signal it"},"lines":{"type":"array","items":{"type":"object","required":["description","amount_cents"],"properties":{"description":{"type":"string"},"amount_cents":{"type":"integer","minimum":1},"account":{"type":"string"}}},"description":"If given, they must add up to amount_cents"},"allow_duplicate":{"type":"boolean"},"document_id":{"type":"string","format":"uuid","description":"The inbox document this bill was read from; it is filed with the bill"},"extraction_id":{"type":"string","format":"uuid","description":"The document.extract result this bill was entered from (its bill_input carries it). The audit log records which fields were corrected."}}}"#,
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
        summary: "Release a bill for payment at its pay step. With a bank connected, this sends the payment through it to the vendor's accepted payee; the preview shows exactly what will be sent. Preview first; commit with the returned token.",
        tier: RiskTier::Confirmed,
        person_only: true,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"},"pay_elsewhere":{"type":"boolean","description":"Release without paying through the connected bank; record the payment later with bill.mark_paid"}}}"#,
    },
    CommandSpec {
        name: "bill.auto_pay",
        summary: "Pay a bill whose workflow pays automatically (status ready_to_pay) through the connected Mercury account, to the vendor's accepted payee. Payvy does this on its own within a minute of the bill getting there; run it to pay one now. If the payment doesn't go, the bill waits for a person to release it and is never retried automatically.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["bill_id"],"properties":{"bill_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "bill.mark_paid",
        summary: "Record that a released or ready bill was paid outside Payvy (a check, a payment made in the bank's own app). Payments sent through the connected bank are recorded on their own. Preview first; commit with the returned token.",
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
    // ── Inbox ───────────────────────────────────────────────────────────────
    CommandSpec {
        name: "inbox.get",
        summary: "What is waiting on the caller: bills a step needs them to approve or release (action approve or release), and payee changes and workflow proposals a person must accept or decline. Agents see it too, to remind people.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    // ── People ──────────────────────────────────────────────────────────────
    CommandSpec {
        name: "person.list",
        summary: "Everyone who can sign in to the web app, with the actor name their actions are recorded under.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "person.add",
        summary: "Let someone sign in to the web app with a link sent to their email. They act as the person named by actor, with every right a person has: approving, releasing payments, accepting payees and workflows.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","required":["email","name"],"properties":{"email":{"type":"string","format":"email"},"name":{"type":"string","description":"Their name, for people"},"actor":{"type":"string","description":"The name their actions are recorded under, a-z 0-9 . _ -; defaults to the part of the email before @"}}}"#,
    },
    CommandSpec {
        name: "person.remove",
        summary: "Stop someone signing in, and sign them out everywhere. Their past actions stay on the record.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","required":["actor"],"properties":{"actor":{"type":"string"}}}"#,
    },
    // ── Bank and payments ───────────────────────────────────────────────────
    CommandSpec {
        name: "bank.connect",
        summary: "Connect a bank with an API token from its dashboard. Mercury pays bills: mode request (default) waits in Mercury for a Mercury admin (Custom token, RequestSendMoney scope); mode direct sends at once (Read and Write token, server IP allowlisted). Rho is always read-only (no mode to choose): it never pays and never replaces the paying bank; create its token in Rho Settings > API > Access Tokens with accounts:read and transactions:read. Payvy keeps tokens encrypted and never holds money or account numbers.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","required":["provider","token"],"properties":{"provider":{"type":"string","enum":["mercury","rho"]},"token":{"type":"string"},"environment":{"type":"string","enum":["production","sandbox"]},"mode":{"type":"string","enum":["request","direct"],"description":"Mercury only: how payments are sent. Rho is always read-only."},"account_id":{"type":"string","description":"The account to pay from (Mercury) or display (Rho), if there is more than one"}}}"#,
    },
    CommandSpec {
        name: "bank.get",
        summary: "The connected banks: connection is the paying bank (Mercury) or null, connections lists every live connection with its capability (pay or read_only).",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "bank.disconnect",
        summary: "Disconnect a bank (the paying one by default, or provider rho for the read-only one). No new payments are sent through a disconnected paying bank; ones already sent are still tracked.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","properties":{"provider":{"type":"string","enum":["mercury","rho"]}}}"#,
    },
    CommandSpec {
        name: "bank.accounts",
        summary: "Accounts at the read-only Rho connection, with balances in cents. Empty object input; refuses when no Rho connection is live.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "bank.transactions",
        summary: "Transactions from the read-only Rho connection: signed amounts in cents (negative is money out), counterparty, status and posted time. Filter by account, date and search text.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{"account_id":{"type":"string"},"posted_after":{"type":"string","description":"ISO 8601, inclusive"},"posted_before":{"type":"string","description":"ISO 8601, exclusive"},"initiated_after":{"type":"string"},"initiated_before":{"type":"string"},"search":{"type":"string","description":"Counterparty, memo or note text"},"limit":{"type":"integer","minimum":1,"maximum":1000}}}"#,
    },
    CommandSpec {
        name: "bank.recipients",
        summary: "Recipients set up in the connected bank, with the ways each can be paid and the last four digits of each account. Use one with vendor.propose_payee.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{"search":{"type":"string","description":"Part of the name"}}}"#,
    },
    CommandSpec {
        name: "payment.list",
        summary: "Payments sent through the bank, newest first, or only those still waiting on the bank.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{"open":{"type":"boolean"},"limit":{"type":"integer","minimum":1,"maximum":500}}}"#,
    },
    CommandSpec {
        name: "payment.refresh",
        summary: "Ask the bank where a payment stands now and record it: sent marks the bill paid; failed, cancelled or rejected sends the bill back for release; returned reopens a paid bill. Payvy does this on its own every minute or two.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["payment_id"],"properties":{"payment_id":{"type":"string","format":"uuid"}}}"#,
    },
    // ── Documents ───────────────────────────────────────────────────────────
    CommandSpec {
        name: "document.upload",
        summary: "Add a file to the inbox: a PDF or image (PNG, JPEG, GIF, WebP, TIFF) of a bill, up to 25 MB, as base64. Its type is read from the file itself. Refused if the same file is already here, naming the existing document.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["filename","content_base64"],"properties":{"filename":{"type":"string"},"content_base64":{"type":"string","description":"The file, base64-encoded (standard alphabet)"}}}"#,
    },
    CommandSpec {
        name: "document.list",
        summary: "Documents in the inbox (default), oldest first; or filed or trashed ones.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{"status":{"type":"string","enum":["inbox","filed","trashed"]},"limit":{"type":"integer","minimum":1,"maximum":500}}}"#,
    },
    CommandSpec {
        name: "document.get",
        summary: "A document's details, every time a model read it (newest first, with checks and the suggested bill_input), and a download link valid for 15 minutes.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","required":["document_id"],"properties":{"document_id":{"type":"string","format":"uuid"}}}"#,
    },
    CommandSpec {
        name: "document.extract",
        summary: "Read an inbox document with the configured model: vendor, invoice number, dates, lines and total, checked against each other and matched to a vendor on file. Returns the checks, whether it is ready, and bill_input: the bill.create input it suggests, to enter as is or corrected. Nothing becomes a bill until bill.create. New documents are read automatically when the server's reader is on. Refused if the document was already read, unless again is true.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["document_id"],"properties":{"document_id":{"type":"string","format":"uuid"},"again":{"type":"boolean","description":"Read it again even though it was already read, e.g. after adding the vendor"}}}"#,
    },
    CommandSpec {
        name: "document.trash",
        summary: "Take a document out of the inbox because it isn't a bill (a statement, a marketing email, a duplicate). The file is kept.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["document_id","reason"],"properties":{"document_id":{"type":"string","format":"uuid"},"reason":{"type":"string"}}}"#,
    },
    // ── Email ───────────────────────────────────────────────────────────────
    CommandSpec {
        name: "email.address",
        summary: "This organization's inbound address. Bills emailed or forwarded to it land in the document inbox; a +tag after the token is allowed (token+fuel@...). Null until a person creates one with email.new_address.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "email.new_address",
        summary: "Create the inbound address, or replace it if it is getting spam. The old address stops working at once, so anyone forwarding to it must switch.",
        tier: RiskTier::Routine,
        person_only: true,
        input_schema: r#"{"type":"object","properties":{}}"#,
    },
    CommandSpec {
        name: "email.import",
        summary: "Read a raw email (an .eml file, base64) into the inbox: its PDF and image attachments become documents, including those of a forwarded email. The inbound webhook uses this too. Refused if the same email is already here. The result lists what was skipped and why.",
        tier: RiskTier::Routine,
        person_only: false,
        input_schema: r#"{"type":"object","required":["raw_base64"],"properties":{"raw_base64":{"type":"string","description":"The whole RFC 5322 message, base64-encoded (standard alphabet)"},"recipient":{"type":"string","description":"The address it was delivered to, if known"}}}"#,
    },
    CommandSpec {
        name: "email.list",
        summary: "Emails received, newest first: sender, subject and what was skipped.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":500}}}"#,
    },
    CommandSpec {
        name: "email.get",
        summary: "One email: its details, the documents it brought in, and a 15-minute download link for the original message.",
        tier: RiskTier::Read,
        person_only: false,
        input_schema: r#"{"type":"object","required":["email_id"],"properties":{"email_id":{"type":"string","format":"uuid"}}}"#,
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
