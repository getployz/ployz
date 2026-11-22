/**
 * The control plane's fenced incident writes, verbatim from the uptime schema contract. The Rust checker runs these
 * exact texts; they live here so the Postgres test proves the fences they rely on.
 */
export const controlPlaneStatements = {
  /** $1 check id, $2 revision, $3 incident id, $4 opened_at, $5 opened_by, $6 alert payload. */
  open: `
  WITH target AS (
    SELECT id, organization_id, revision FROM uptime_check
    WHERE id = $1 AND revision = $2 AND enabled
    FOR SHARE),
  opened AS (
    INSERT INTO uptime_incident (id, organization_id, check_id, check_revision, opened_at, opened_by)
    SELECT $3, organization_id, id, revision, $4, $5 FROM target
    ON CONFLICT (check_id) WHERE closed_at IS NULL DO NOTHING
    RETURNING id, organization_id),
  alerted AS (
    INSERT INTO uptime_alert (organization_id, incident_id, kind, payload)
    SELECT organization_id, id, 'opened', $6 FROM opened)
  SELECT count(*)::int AS applied FROM opened;`,
  /** $1 incident id, $2 revision, $3 closed_at, $4 closed_by, $5 alert payload. */
  observedClose: `
  WITH closed AS (
    UPDATE uptime_incident i SET closed_at = $3, closed_by = $4
    WHERE i.id = $1 AND i.check_revision = $2 AND i.closed_at IS NULL
      AND EXISTS (SELECT 1 FROM uptime_check c WHERE c.id = i.check_id AND c.enabled AND c.revision = $2 FOR SHARE)
    RETURNING i.id, i.organization_id),
  alerted AS (
    INSERT INTO uptime_alert (organization_id, incident_id, kind, payload)
    SELECT organization_id, id, 'resolved', $5 FROM closed
    ON CONFLICT (incident_id, kind) DO NOTHING)
  SELECT count(*)::int AS applied FROM closed;`,
} as const;
