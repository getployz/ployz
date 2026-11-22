import { sql } from "drizzle-orm";
import type { UptimeExpect } from "./tables";

/**
 * A config edit or disable, in one statement: bump the revision and close the open incident with a non-recovery
 * reason, writing no alert. A rename alone doesn't bump or close, so it doesn't use this.
 */
export const editUptimeCheckStatement = (input: {
  readonly id: string;
  readonly organizationId: string;
  readonly url: string;
  readonly expect: UptimeExpect;
  readonly intervalSeconds: number;
  readonly timeoutMs: number;
  readonly enabled: boolean;
}) => sql`
  WITH edited AS (
    UPDATE uptime_check SET url = ${input.url}, expect = ${input.expect}, interval_seconds = ${input.intervalSeconds}, timeout_ms = ${input.timeoutMs}, enabled = ${input.enabled},
           revision = revision + 1, updated_at = now()
    WHERE id = ${input.id} AND organization_id = ${input.organizationId}
    RETURNING id, enabled),
  closed AS (
    UPDATE uptime_incident i SET closed_at = now(),
           closed_by = jsonb_build_object('kind', CASE WHEN e.enabled THEN 'check_changed' ELSE 'check_disabled' END)
    FROM edited e WHERE i.check_id = e.id AND i.closed_at IS NULL
    RETURNING i.id)
  SELECT * FROM edited`;
