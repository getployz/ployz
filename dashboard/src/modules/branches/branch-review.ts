import { branchChanges, type BranchReason, type BranchRow as ChangeRow } from "@ployz/sdk/config";
import { asRecord, asString } from "#/lib/json";
import type { SavedEnvironmentIntent } from "#/modules/environment-design/saved-intent";
import { presentSettingChange } from "#/modules/services/service-deployment-diff/fields";
import { liveNodeUsers, liveOwner } from "./live-owner";

export type { ChangeRow };
type RowValue = ChangeRow["from"];

/** Both sides of a Branch's relationship with its Parent, as the browser has them: redacted, secrets as fingerprints. */
export type BranchReviewInput = {
  /** The Branch row's base: what the two last shared. */
  base: SavedEnvironmentIntent;
  kept: boolean;
  branch: SavedEnvironmentIntent;
  /** The Destination's Working State. The Destination is the Parent. */
  parent: SavedEnvironmentIntent;
  /** The Parent's Applied State in authored form; null before its first deploy. */
  parentApplied: SavedEnvironmentIntent | null;
  /** Managed-hostname suffixes: `-<branch>` for a Branch, "" for a root. */
  hostnames: { branch: string; parent: string };
};

/** A Live Node whose owner deployed it after the Branch last deployed: deploying the Branch picks up its new values. */
export type LiveUpdate = { lineageId: string; ownerEnvironmentId: string; deployedAt: Date };

export type BranchReview = {
  /** Branch Working → Destination Working, against the base. */
  merge: ChangeRow[];
  /** Parent Applied → Branch Working, against the base. */
  update: ChangeRow[];
  /** Settings each side keeps as its own, from the merge comparison. */
  differ: ChangeRow[];
  /** Core's review string of the merge rows; Merge sends it, and the server refuses it once the rows moved. */
  mergeReview: string;
};

const moves = (rows: ChangeRow[]) => rows.filter((row) => row.role === "move");

/**
 * The review page's rows, from core's branchChanges (compare only, no picks). The server reruns the same comparison
 * from authoritative states before applying picks.
 */
export function branchReview(input: BranchReviewInput): BranchReview {
  const { rows: merge, review: mergeReview } = branchChanges(mergeInput(input));
  const update = input.parentApplied ? branchChanges(updateInput(input, input.parentApplied)).rows : [];
  return { merge: moves(merge), update: moves(update), differ: merge.filter((row) => row.role === "differ"), mergeReview };
}

export const mergeInput = (input: BranchReviewInput) => ({
  base: input.base, from: input.branch, into: input.parent, parent: input.parentApplied ?? undefined,
  provided: usedLive(input.parent), hostnames: { from: input.hostnames.branch, into: input.hostnames.parent }, fromKept: input.kept,
});

export const updateInput = (input: BranchReviewInput, parentApplied: SavedEnvironmentIntent) => ({
  base: input.base, from: parentApplied, into: input.branch,
  provided: usedLive(input.branch), hostnames: { from: input.hostnames.parent, into: input.hostnames.branch }, fromKept: false,
});

/** Lineages an Environment's variables reference but it doesn't own: the nodes it uses live. */
export const usedLive = (intent: SavedEnvironmentIntent) => [...liveNodeUsers(intent).keys()];

/**
 * Each Live Node the owner redeployed after the Branch's latest deploy. The owner is `liveOwner` of the Branch's Parent.
 * `deployedAt` holds each Environment's Applied lineages and when they last deployed.
 */
export function liveUpdates(input: {
  live: string[];
  parentId: string;
  branches: Iterable<{ environmentId: string; parentEnvironmentId: string }>;
  branchDeployedAt: Date | null;
  deployedAt: ReadonlyMap<string, Record<string, Date>>;
}): LiveUpdate[] {
  const since = input.branchDeployedAt;
  if (!since) return [];
  const applied = new Map([...input.deployedAt].map(([id, at]) => [id, new Set(Object.keys(at))]));
  return input.live.flatMap((lineageId) => {
    const ownerEnvironmentId = liveOwner(input.parentId, lineageId, input.branches, applied);
    const deployedAt = ownerEnvironmentId ? input.deployedAt.get(ownerEnvironmentId)?.[lineageId] : undefined;
    return ownerEnvironmentId && deployedAt && deployedAt > since ? [{ lineageId, ownerEnvironmentId, deployedAt }] : [];
  });
}

/** The latest of an Environment's Applied deploy times, or null before its first deploy. */
export function latestDeploy(deployedAt: Record<string, Date> | undefined): Date | null {
  const times = Object.values(deployedAt ?? {});
  return times.length ? new Date(Math.max(...times.map((time) => time.getTime()))) : null;
}

export const rowLineage = (row: { key: string }) => row.key.slice(0, row.key.indexOf(":"));
const rowPath = (row: ChangeRow) => row.key.slice(row.key.indexOf(":") + 1);

/** A row in words: which setting of which node, and its value on each side. */
export type PresentedRow = { key: string; lineageId: string; node: string; label: string; before: string; after: string };

/** `before` is the receiver's value, `after` the one that would land. */
export function presentRow(row: ChangeRow, nameOf: (lineage: string) => string): PresentedRow {
  const lineageId = rowLineage(row);
  const path = rowPath(row);
  const value = (side: RowValue) => display(path, side, nameOf);
  const label = path === "node" ? (row.role === "move" ? "New" : "")
    : path === "name" ? "Name"
    : path === "data" ? "Data"
    : path.startsWith("mounts.") ? `Mount of ${nameOf(path.slice(7))}`
    : path.startsWith("variables.") ? path.slice(10)
    : presentSettingChange("service", path, null, null).label;
  return { key: row.key, lineageId, node: nameOf(lineageId), label, before: value(row.into), after: value(row.from) };
}

function display(path: string, value: RowValue, nameOf: (lineage: string) => string): string {
  if (value === null || value === undefined) return "";
  if (path.startsWith("variables.")) {
    const record = asRecord(value);
    if (record?.["kind"] === "secret") return "hidden";
    if (record?.["kind"] === "literal") return asString(record["value"]) ?? "";
    return Array.isArray(record?.["parts"]) ? record["parts"].map((part) => {
      const piece = asRecord(part);
      if (piece?.["kind"] === "text") return asString(piece["value"]) ?? "";
      const owner = asRecord(piece?.["owner"]);
      return `\${{ ${owner?.["scope"] === "service" ? nameOf(asString(owner["lineageId"]) ?? "") : "self"}.${asString(piece?.["key"])} }}`;
    }).join("") : "";
  }
  if (path === "source.repository") return asString(asRecord(value)?.["repository"]) ?? "";
  if (path === "source.credentials") return "Configured";
  if (path === "routes") return Array.isArray(value) ? value.join(", ") || "None" : "";
  // A node or data row names the node, which the row's title already says.
  if (path === "node" || path === "data") return "";
  if (path === "name" || path.startsWith("mounts.")) return asString(value) ?? "";
  return presentSettingChange("service", path, null, value).newValue;
}

/** Why a row is meant to differ, in words. */
export const DIFFER_REASONS = {
  live: "Used live, not copied",
  left_out: "Left out of this branch",
  sizing: "Replicas and resource limits are sized per environment",
  custom_domain: "Custom domains stay with each environment",
  generated_address: "Each environment gets its own web address",
  git_branch: "Each environment deploys its own Git branch",
  data: "Volume size and data stay with each environment",
} satisfies Record<BranchReason, string>;
