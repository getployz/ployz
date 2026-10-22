-- The user who admitted a Deployment, by the immutable id Cloud authenticated (`Trusted.user`): what makes it
-- someone's own when they cancel it. `admitted_by` is a display name, which may repeat. NULL for one Cloud's own
-- automation admitted, and for one admitted before, which reads as nobody's.
ALTER TABLE config_deployment ADD COLUMN admitted_by_user TEXT;
