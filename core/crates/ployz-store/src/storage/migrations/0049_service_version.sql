-- Every signed version a client posted, as the JSON of a Signed<ServiceVersion>, under
-- the Namespace, Service and digest its own body names, keyed by the SHA-256 of that
-- JSON so no later post replaces a kept one. A runner or a drain sends each Server the
-- newest rows for the digest of the spec it asks for; the Server checks them. Nothing
-- prunes them: a Server ignores a signer it no longer trusts.
CREATE TABLE config_service_version (
    organization_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    service TEXT NOT NULL,
    digest TEXT NOT NULL,
    statement_hash TEXT NOT NULL,
    statement TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, namespace, service, digest, statement_hash)
);
-- What each Service of this Deployment runs, by runtime name, and where a Service it
-- names nothing for takes its pin at claim: {"origin": "own"} for a Deploy,
-- {"origin": "replay", "source": id} for a retry or rollback, or {"origin": "legacy"}
-- for a Deployment admitted before this column, with
-- "services": {service: {digest, pin?: {tag, image}}}. NULL is a Deploy nothing posted
-- versions for or claimed yet.
ALTER TABLE config_deployment ADD COLUMN ran TEXT;
UPDATE config_deployment SET ran = '{"origin":"legacy","services":{}}';
