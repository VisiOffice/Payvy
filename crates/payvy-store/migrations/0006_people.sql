-- People who sign in to the web app with a link sent to their email. Each
-- person acts as the actor of the same name, with the same rights as a person
-- API key. Like api_keys, these tables have no row-level security: a person,
-- link or session is looked up before its organization is known, so every
-- query here names the organization itself.

CREATE TABLE people (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations(id),
    -- The actor name events record, e.g. robert. Lowercase a-z 0-9 . _ -
    actor text NOT NULL CHECK (actor ~ '^[a-z0-9][a-z0-9._-]{0,63}$'),
    -- Stored lowercase.
    email text NOT NULL CHECK (email = lower(email) AND email LIKE '%_@_%'),
    name text NOT NULL,
    created_at timestamptz NOT NULL,
    created_by text NOT NULL,
    removed_at timestamptz,
    removed_by text
);
CREATE UNIQUE INDEX people_actor ON people (org_id, actor) WHERE removed_at IS NULL;
CREATE UNIQUE INDEX people_email ON people (org_id, email) WHERE removed_at IS NULL;
CREATE INDEX people_by_email ON people (email) WHERE removed_at IS NULL;

-- A sign-in link: only a SHA-256 of its token is stored. One use, short-lived.
CREATE TABLE sign_in_links (
    token_hash text PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES people(id),
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at timestamptz
);
CREATE INDEX sign_in_links_person ON sign_in_links (person_id, created_at);

-- A signed-in browser: only a SHA-256 of the cookie value is stored.
CREATE TABLE sessions (
    token_hash text PRIMARY KEY,
    person_id uuid NOT NULL REFERENCES people(id),
    created_at timestamptz NOT NULL,
    expires_at timestamptz NOT NULL,
    ended_at timestamptz
);
CREATE INDEX sessions_person ON sessions (person_id);
