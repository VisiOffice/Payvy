-- The invitation checker asks Mercury about open invitations every few
-- minutes; this records when each was last asked, so one stuck invitation
-- doesn't crowd out the rest.
ALTER TABLE vendor_payee_invites ADD COLUMN checked_at timestamptz;
