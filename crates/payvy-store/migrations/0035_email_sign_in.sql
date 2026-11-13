-- A sign-in link proves an email address, not a membership: one link per
-- address, however many companies it belongs to. Opening it signs in
-- straight away when the address belongs to one company (or to the company
-- the link was sent for); otherwise it yields a short-lived choice ticket,
-- and the person picks a company from the ones the address belongs to then.
-- Membership is checked when the session starts, never when the link is made.
--
-- Like sign_in_links and sessions, this table has no row-level security: it
-- is used before any organization is known. Only SHA-256 hashes are stored.

CREATE TABLE email_proofs (
    token_hash text PRIMARY KEY,
    -- 'link': emailed, opened once. 'choice': issued by opening a link to an
    -- address with several companies, used once to pick one.
    kind text NOT NULL CHECK (kind IN ('link', 'choice')),
    -- Stored lowercase.
    email text NOT NULL CHECK (email = lower(email) AND email LIKE '%_@_%'),
    -- The company a link was sent for (signup, an invitation); it is opened
    -- there when the address still belongs to it.
    org_id uuid REFERENCES organizations(id),
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at timestamptz
);
CREATE INDEX email_proofs_email ON email_proofs (email, created_at);
