-- The app connects as a plain role: superusers bypass row-level security,
-- which would hide organization-isolation bugs in development.
CREATE ROLE payvy LOGIN PASSWORD 'payvy' NOSUPERUSER NOBYPASSRLS CREATEDB;
CREATE DATABASE payvy OWNER payvy;
