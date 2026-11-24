-- A vendor that keeps one of the company's cards on file and charges it
-- (docs/cards.md). Payvy keeps the bank's card id and the last four only,
-- never the card number. A person links a card; rows only move
-- active -> removed, so the log says which card was on file when.
CREATE TABLE vendor_cards (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    connection_id uuid NOT NULL REFERENCES bank_connections(id),
    card_id text NOT NULL,
    last_four text NOT NULL,
    nickname text,
    kind text NOT NULL CHECK (kind IN ('debit', 'credit')),
    -- Charges on the card from this day on are matched to this vendor's
    -- bills; earlier ones are history Payvy leaves alone.
    charges_from date NOT NULL,
    status text NOT NULL CHECK (status IN ('active', 'removed')),
    reason text,
    linked_by text NOT NULL,
    linked_at timestamptz NOT NULL,
    removed_by text,
    removed_at timestamptz
);
-- One card per vendor, and one vendor per card: a card shared by several
-- vendors can't say which bill a charge pays.
CREATE UNIQUE INDEX vendor_cards_one_per_vendor ON vendor_cards (org_id, vendor_id) WHERE status = 'active';
CREATE UNIQUE INDEX vendor_cards_one_vendor_per_card ON vendor_cards (org_id, connection_id, card_id) WHERE status = 'active';

ALTER TABLE vendor_cards ENABLE ROW LEVEL SECURITY;
ALTER TABLE vendor_cards FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON vendor_cards
    USING (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid)
    WITH CHECK (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid);

-- Charges the bank reports on a linked card, as the bank reports them
-- (amount_cents is the charge, positive). A charge pays at most one bill
-- and a bill is paid by at most one charge. bill_id set by the matcher
-- (rule 'amount') or a person (rule 'person'); dismissed by a person when
-- it pays no bill (a fee, a mistake).
CREATE TABLE card_charges (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    connection_id uuid NOT NULL REFERENCES bank_connections(id),
    vendor_card_id uuid NOT NULL REFERENCES vendor_cards(id),
    vendor_id uuid NOT NULL REFERENCES vendors(id),
    transaction_id text NOT NULL,
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    status text NOT NULL CHECK (status IN ('pending', 'posted', 'reversed')),
    counterparty text,
    charged_at timestamptz NOT NULL,
    posted_at timestamptz,
    link text,
    bill_id uuid REFERENCES bills(id),
    rule text CHECK (rule IN ('amount', 'person')),
    matched_by text,
    matched_at timestamptz,
    dismissed_by text,
    dismissed_at timestamptz,
    dismissed_reason text,
    created_at timestamptz NOT NULL,
    updated_at timestamptz NOT NULL,
    CHECK ((bill_id IS NULL) = (rule IS NULL)),
    CHECK (bill_id IS NULL OR dismissed_at IS NULL)
);
CREATE UNIQUE INDEX card_charges_once ON card_charges (org_id, connection_id, transaction_id);
CREATE UNIQUE INDEX card_charges_one_per_bill ON card_charges (org_id, bill_id) WHERE bill_id IS NOT NULL;
CREATE INDEX card_charges_by_vendor ON card_charges (org_id, vendor_id, charged_at);

ALTER TABLE card_charges ENABLE ROW LEVEL SECURITY;
ALTER TABLE card_charges FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON card_charges
    USING (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid)
    WITH CHECK (org_id = NULLIF(current_setting('payvy.org_id', true), '')::uuid);
