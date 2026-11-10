-- Mercury tells Payvy about payment transactions through a webhook Payvy
-- registers when the connection is made. Its signing secret is sealed like
-- the token; losing it only loses push, since the payment checker still polls.
ALTER TABLE bank_connections ADD COLUMN webhook_id text;
ALTER TABLE bank_connections ADD COLUMN webhook_secret_sealed bytea;
