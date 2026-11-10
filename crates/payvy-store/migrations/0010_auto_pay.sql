-- Set when a payment for the bill didn't go, or came back. From then on an
-- automatic pay step waits for a person to release the bill, so auto-pay
-- never retries on its own.
ALTER TABLE bills ADD COLUMN release_by_person boolean NOT NULL DEFAULT false;
