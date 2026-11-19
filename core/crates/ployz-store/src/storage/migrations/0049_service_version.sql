-- Every signed version a client posted, as the JSON of a Signed<ServiceVersion>, under
-- the Namespace, Service and digest its own body names. A runner or a drain sends each
-- Server the rows for the digest of the spec it asks for; the Server checks them. A
-- later signature by the same signer replaces its row. Nothing prunes them: a Server
-- ignores a signer it no longer trusts.
CREATE TABLE config_service_version (
    organization_id TEXT NOT NULL,
    namespace TEXT NOT NULL,
    service TEXT NOT NULL,
    digest TEXT NOT NULL,
    signer TEXT NOT NULL,
    statement TEXT NOT NULL,
    created_at BIGINT NOT NULL,
    PRIMARY KEY (organization_id, namespace, service, digest, signer)
);
-- What each Service of this Deployment runs, by runtime name: the digest of its pinned
-- spec and the pin, as {service: {digest, pin?: {tag, image}}}. NULL until a client
-- posts versions or a runner claims it.
ALTER TABLE config_deployment ADD COLUMN ran TEXT;
