# Company roles and permissions

People can hold multiple roles. The command registry declares a permission for
**every** command; the shared dispatcher enforces it for REST, MCP, preview and
commit. Workflow assignments, person-only restrictions, confirmation tokens,
auto-pay limits, and separation of duties remain additional requirements.

| Role | What it permits |
| --- | --- |
| Admin | Full access: people/roles, company and bank settings, workflows, approval rules, auto-pay limits, preparation, approvals, payments, and voiding bills. Named workflow assignments still apply. |
| AP clerk | Upload/read/arrange/set aside documents, create bills, maintain vendors/account numbers, propose payee mappings, import email, and report workflow signals. No approval, payment release, or configuration changes. |
| Approver | Approve eligible assigned steps and reject open unpaid bills. No payment release. |
| Payment releaser | Release eligible payments, mark released bills paid elsewhere, refresh payment status, invite/create recipients, and accept/decline payee changes. No workflow or bank-connection changes. |

All roles can read company records, including invoices, vendors, people, bank
read data, and events. This version does not restrict which bills someone can
see, provide custom roles, or provide a separate read-only role. Add both
Approver and Payment releaser when one person needs both capabilities.

## People and identity

Settings > People lets an admin select roles when adding someone and edit them
later. New app/API additions default to AP clerk. Empty, duplicate and unknown
roles are rejected. `person.set_roles` is person-only and Admin-only, records
`person.roles_changed` with old/new roles, supports preview, and rejects a no-op.

Roles belong to the company/actor identity used by the audit log. Session cookies
and person API keys both consult that identity's current roles on **each command**.
A role removed after a payment preview is checked again at commit. A request
already authorized and running may finish; changing roles does not cancel bank
requests in flight. The browser refreshes access on navigation, focus, writes,
a denied action, and every 30 seconds. Server enforcement does not wait for it.

Membership and role changes serialize within the company, then recheck the
caller's access. Concurrent admins cannot demote each other using authority they
have just lost. The last admin who can sign in cannot be demoted or removed.
Self-removal remains prohibited. Removal ends sessions, removes the role grant,
and revokes that actor's person API keys. Adding them again does not revive those
keys. Agent keys are separate automation identities and are not revoked by this.

The bootstrap `PAYVY_API_KEY` remains a trusted server-operator credential, scoped
to the default company, for setup/recovery. This is an explicit credential path:
an ordinary person named `admin` gets no special rights. The operator CLI can add
an admin with `payvy person add --email ... --name ... --roles admin` (Admin is the
CLI default). New person API keys require an existing company role grant; create
the person first. Neither a key nor a session carries authority to assign roles.

## Approvers named in workflows

A workflow's approve steps name people, and a named person who can't approve
would strand every bill reaching their step (bills keep the workflow version
they arrived under). So `workflow.propose` refuses a workflow naming a person
in the company who lacks the Approver (or Admin) role, and `workflow.accept`
rechecks — roles may have changed since the proposal. Naming someone who
isn't a person here yet is allowed; they may be added under that name later.

Taking the Approver role away (`person.set_roles`) or removing the person
(`person.remove`) is refused exactly while an open bill is currently waiting
on their approval step, and only then: settle those bills first (they can
approve or reject them). At other times role changes stay possible, and
queued notifications still recheck the recipient's roles before sending.

## Agents and automated payments

Agent capabilities are unchanged: registry `person_only` gates still prohibit
approving, releasing, accepting workflows/payees, or administering people.
Existing preparation and background commands remain available. In particular,
auto-pay continues under the already accepted workflow and company/vendor limit;
it does not acquire a person's payment role or bypass any payment checks.

MCP tools/list and the REST command list are filtered to the current caller.
The signed-in session response contains roles and allowed command names for UI
rendering. Those lists are hints; the server authorizes each invocation afresh.
The inbox only counts actions the caller is permitted to take. Workflow selectors
only offer eligible people for approval and release; existing assignments remain
visible when a person loses the required role, so an admin can revise the workflow.
Already-created bills keep their pinned workflow. Roles do not automatically
reassign a bill, and already-recorded approvals continue to count.

## Rollout

Migration `0017_roles.sql` gives **existing** active people and legacy active
person-key identities Admin, preserving the access they had before roles existed.
This is a compatibility choice, not automatic least-privilege assignment: review
People and reduce each person's roles after deployment. Legacy person-key identities
without a sign-in entry are not shown in People; add them with the same actor name
and the intended roles to manage that identity there. Removed identities are
not granted access again, and person keys left active by the old removal path are
revoked so re-adding those people cannot revive forgotten keys. New app/API additions default to AP clerk.

Deploy the backend/migration before the frontend, then check People with an admin
session. Keep at least one sign-in admin. This does not change the separate
approval rule about who entered/uploaded a bill; extending that rule to payment
release remains future work.
