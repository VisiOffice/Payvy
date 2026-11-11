-- The actor is the same company-scoped identity used by sessions, keys and
-- the audit log. Preserve existing access, including legacy person API keys.
CREATE TABLE actor_roles (
    org_id uuid NOT NULL REFERENCES organizations(id),
    actor text NOT NULL,
    roles text[] NOT NULL CHECK (cardinality(roles) BETWEEN 1 AND 4 AND roles <@ ARRAY['admin','ap_clerk','approver','payment_releaser']::text[]),
    PRIMARY KEY (org_id, actor)
);
-- Older person.remove ended sessions but did not revoke person keys. Revoke
-- those now, so re-adding a removed identity cannot revive a forgotten key.
UPDATE api_keys k SET revoked_at = (
    SELECT max(p.removed_at) FROM people p WHERE p.org_id = k.org_id AND p.actor = k.actor
)
WHERE k.kind = 'person' AND k.revoked_at IS NULL
AND EXISTS (SELECT 1 FROM people p WHERE p.org_id = k.org_id AND p.actor = k.actor AND p.removed_at IS NOT NULL)
AND NOT EXISTS (SELECT 1 FROM people p WHERE p.org_id = k.org_id AND p.actor = k.actor AND p.removed_at IS NULL);
INSERT INTO actor_roles (org_id, actor, roles)
SELECT org_id, actor, ARRAY['admin']::text[] FROM people WHERE removed_at IS NULL
UNION
SELECT k.org_id, k.actor, ARRAY['admin']::text[] FROM api_keys k
WHERE k.kind = 'person' AND k.revoked_at IS NULL
AND NOT EXISTS (SELECT 1 FROM people p WHERE p.org_id = k.org_id AND p.actor = k.actor AND p.removed_at IS NOT NULL)
ON CONFLICT DO NOTHING;
ALTER TABLE actor_roles ENABLE ROW LEVEL SECURITY;
ALTER TABLE actor_roles FORCE ROW LEVEL SECURITY;
CREATE POLICY org_isolation ON actor_roles
    USING (org_id = current_setting('payvy.org_id', true)::uuid)
    WITH CHECK (org_id = current_setting('payvy.org_id', true)::uuid);
