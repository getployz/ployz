/**
 * One line per request, in a log Cloud cannot reach. It records what was asked and what GitHub granted, never a
 * token, a code, a secret or a request header.
 */
export interface AuditLine {
  time: string;
  caller: string;
  route: string;
  outcome: "granted" | "refused" | "failed";
  reason?: string;
  installation_id?: number;
  repository_ids?: number[];
  permissions_asked?: Record<string, unknown>;
  permissions_granted?: Record<string, string>;
  ref?: string;
  head_sha?: string;
  workflow_digest?: string;
  run_id?: number;
  check_name?: string;
  github_status?: number;
  token_expires_at?: string;
  duration_ms?: number;
}

/** What a route adds to its request's line as it learns it. */
export type AuditFacts = Omit<Partial<AuditLine>, "time" | "caller" | "route" | "outcome" | "reason" | "duration_ms">;

export type AuditSink = (line: AuditLine) => void;

export const stdoutAudit: AuditSink = (line) => {
  process.stdout.write(`${JSON.stringify(line)}\n`);
};
