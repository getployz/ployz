import { createHash } from "node:crypto";
import canonicalWorkflows from "./canonical-workflows.json" with { type: "json" };

/**
 * The only permissions Cloud may hold in a token. A request outside them is refused, never trimmed: Cloud's code
 * never asks for more, so a request that does is the signal worth an alarm.
 */
export const READ_SET = { contents: "read", metadata: "read", pull_requests: "read", checks: "read" } as const;

export type ReadPermission = keyof typeof READ_SET;
export type ReadPermissions = Partial<Record<ReadPermission, "read">>;

/** The one workflow the minter reads, dispatches, lists and cancels runs of. */
export const BUILD_WORKFLOW_FILE = "ployz-build.yml";
export const BUILD_WORKFLOW_PATH = `.github/workflows/${BUILD_WORKFLOW_FILE}`;

/** Inputs Cloud may set on a dispatch. `cloud` is not among them: the minter fills it from its own config. */
export const BUILD_INPUTS = ["build", "runner", "commit"] as const;

/** Copied from `PR_CHECK_NAME` in `dashboard/src/modules/config-store/store-pull-requests.ts`, never imported. */
export const PLOYZ_CHECK_NAMES: ReadonlySet<string> = new Set(["Ployz · ready to merge"]);

/** The canonical `ployz-build.yml` digests the deployed minter dispatches. Lanes may swap in their own set. */
export const CANONICAL_DIGESTS: ReadonlySet<string> = new Set(canonicalWorkflows.digests);

/** SHA-256 of a workflow file after CRLF becomes LF, as hex. */
export function workflowDigest(bytes: Uint8Array): string {
  const text = Buffer.from(bytes).toString("latin1").replaceAll("\r\n", "\n");
  return createHash("sha256").update(Buffer.from(text, "latin1")).digest("hex");
}

/**
 * The permissions to send GitHub for a Cloud token request, or the reason it is refused. No permissions means the
 * whole read set, sent explicitly so GitHub never falls back to everything the installation accepted.
 */
export function readTokenPermissions(asked: Readonly<Record<string, unknown>> | undefined):
  { ok: true; permissions: ReadPermissions } | { ok: false; reason: string } {
  if (asked === undefined) return { ok: true, permissions: { ...READ_SET } };
  const entries = Object.entries(asked);
  if (entries.length === 0) return { ok: false, reason: "permissions is empty" };
  const permissions: ReadPermissions = {};
  for (const [name, level] of entries) {
    if (!Object.hasOwn(READ_SET, name)) return { ok: false, reason: `permission ${name} is outside the read set` };
    if (level !== "read") return { ok: false, reason: `permission ${name} must be read` };
    permissions[name as ReadPermission] = "read";
  }
  return { ok: true, permissions };
}
