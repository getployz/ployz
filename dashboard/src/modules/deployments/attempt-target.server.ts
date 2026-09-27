import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { JsonObject } from "#/db/tables";
import { asRecord, asString } from "#/lib/json";
import { canonicalJson } from "#/modules/environment-design/canonical-json";
import { compareNodeSettings, parseNodeConfigs } from "#/modules/environment-design/environment-change-set";
import { environmentNodeConfigSnapshot } from "#/modules/runtime/tables";
import { presentSettingChange } from "#/modules/services/service-deployment-diff/fields";
import { Database } from "#/server/database.server";
import type { TargetNodeList } from "./deployment-contract";
import { environmentDeployment } from "./tables";

type Node = { nodeType: "service" | "volume"; nodeId: string; config: JsonObject };

/**
 * A snapshot's node facts for the target node list. Raw JSON reads: an applied config may predate today's config schema.
 * `name` is a service's private DNS name (its Image Build and runtime service name) or a volume's name; only a Git-sourced
 * service builds an image.
 */
export function snapshotNodeFacts({ nodeType, nodeId, config }: Node): Omit<TargetNodeList["nodes"][number], "changed" | "removed"> {
  const record = asRecord(config);
  const source = nodeType === "service" ? asRecord(record?.["source"]) : null;
  const kind = source?.["type"];
  const label = asString(source?.[kind === "git" ? "repository" : "image"]);
  const mounts = record?.["mounts"];
  return {
    nodeId, nodeType,
    name: asString(record?.[nodeType === "service" ? "privateDns" : "name"]) ?? "",
    needsBuild: kind === "git",
    source: (kind === "git" || kind === "image") && label ? { kind, label } : null,
    mounts: (Array.isArray(mounts) ? mounts : []).flatMap((mount) => asString(asRecord(mount)?.["volumeResourceId"]) ?? []),
  };
}

/** An updated node's setting changes as presented strings, so sealed values never leave the server; none when either config no longer parses. */
function settingRows({ nodeType, config }: Node, before: Node): TargetNodeList["nodes"][number]["settings"] {
  let configs: ReturnType<typeof parseNodeConfigs>;
  try {
    configs = parseNodeConfigs(nodeType, config, before.config);
  } catch {
    // An applied config may predate today's config schema. Comparing and presenting parsed configs must not fail.
    return undefined;
  }
  return compareNodeSettings(configs).map((row) =>
    ({ path: row.path, kind: row.kind, ...presentSettingChange(nodeType, row.path, row.before, row.after) }));
}

/**
 * Writes the attempt's target node list: each of its snapshots diffed against `applied` (Applied State's nodes by
 * `nodeType:nodeId`) with an updated node's setting changes, plus the applied nodes it drops (Removed). The list is provisional while the attempt is queued and
 * frozen with the Attempt Target when the attempt starts, rewritten against Applied State at that moment.
 * Runs inside a transaction that holds the environment's queue lock.
 */
export const writeTargetNodeList = Effect.fn("Deployments.writeTargetNodeList")(function* (
  environmentDeploymentId: string, applied: ReadonlyMap<string, Node>,
) {
  const { drizzle } = yield* Database;
  const target = yield* drizzle.select({ nodeType: environmentNodeConfigSnapshot.nodeType, nodeId: environmentNodeConfigSnapshot.nodeId, config: environmentNodeConfigSnapshot.config })
    .from(environmentNodeConfigSnapshot).where(eq(environmentNodeConfigSnapshot.environmentDeploymentId, environmentDeploymentId));
  const targetKeys = new Set(target.map((node) => `${node.nodeType}:${node.nodeId}`));
  const targetNodes: TargetNodeList = {
    version: 1,
    nodes: [
      ...target.map((node) => {
        const before = applied.get(`${node.nodeType}:${node.nodeId}`);
        const facts = snapshotNodeFacts(node);
        const changed = facts.needsBuild || !before || canonicalJson(before.config) !== canonicalJson(node.config);
        return { ...facts, changed, removed: false, settings: changed && before ? settingRows(node, before) : undefined };
      }),
      ...[...applied].filter(([key]) => !targetKeys.has(key)).map(([, node]) => ({ ...snapshotNodeFacts(node), changed: true, removed: true, needsBuild: false, mounts: [] })),
    ],
  };
  yield* drizzle.update(environmentDeployment).set({ targetNodes }).where(eq(environmentDeployment.id, environmentDeploymentId));
});
