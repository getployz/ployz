import type { JsonObject } from "#/db/tables";
import { compareResourceSettings, compareServiceSettings, parseResourceConfig, parseServiceConfig, type ServiceSettingChange } from "@ployz/sdk/config";
import type { EnvironmentResourceNodeConfigByType } from "./environment-resource-node";
import type { ServiceDeploymentConfig } from "./services";

export type EnvironmentNodeLifecycle = "create" | "update" | "delete" | "none";
export type EnvironmentNodeIdentity = { type: "service" | "volume"; id: string };
export type EnvironmentNodeConfigByType = { service: ServiceDeploymentConfig } & EnvironmentResourceNodeConfigByType;
export type EnvironmentNodeProjection = {
  [T in EnvironmentNodeIdentity["type"]]: {
    node: EnvironmentNodeIdentity & { type: T };
    config: EnvironmentNodeConfigByType[T] | null;
  };
}[EnvironmentNodeIdentity["type"]];
export type EnvironmentNodeIntroductionProjection = {
  [T in EnvironmentNodeIdentity["type"]]: {
    node: EnvironmentNodeIdentity & { type: T };
    config: EnvironmentNodeConfigByType[T];
  };
}[EnvironmentNodeIdentity["type"]];
export type EnvironmentStateProjection = { token: string; nodes: EnvironmentNodeProjection[] };
export type EnvironmentNodeIntroductionsProjection = { token: string; nodes: EnvironmentNodeIntroductionProjection[] };
/** Head is `submitted ?? applied`; only unsaved, unapplied nodes can reset to Introduction. */
export type EnvironmentChangeSetProjectionInput = {
  working: EnvironmentStateProjection;
  applied: EnvironmentStateProjection;
  saved: EnvironmentStateProjection | null;
  submitted: EnvironmentStateProjection | null;
  nodeIntroductions: EnvironmentNodeIntroductionsProjection;
};

export type DashboardReviewNodeChange = {
  node: EnvironmentNodeIdentity;
  lifecycle: "create" | "update" | "delete";
  comparison: "head" | "introduction" | null;
  settings: ServiceSettingChange[];
};
export type DashboardReviewChangeSet = {
  groups: DashboardReviewNodeChange[];
  totalCount: number;
  headToken: string;
};

type NodeConfig = NonNullable<EnvironmentNodeProjection["config"]> | JsonObject;
/** A state's nodes as `compare` reads them: any config it can parse. */
type ComparedState = { nodes: ReadonlyArray<{ node: EnvironmentNodeIdentity; config: NodeConfig | null }> };
function nodeMap(state: ComparedState) {
  return new Map(state.nodes.map((entry) => [`${entry.node.type}:${entry.node.id}`, entry]));
}
/** A node's current and baseline configs through today's config schema; throws when either no longer parses. */
export function parseNodeConfigs(type: EnvironmentNodeIdentity["type"], current: NodeConfig, baseline: NodeConfig) {
  return type === "service"
    ? { type, current: parseServiceConfig(current), baseline: parseServiceConfig(baseline) }
    : { type, current: parseResourceConfig("volume", current), baseline: parseResourceConfig("volume", baseline) };
}
/** A node's setting changes from baseline to current, as the review lists them; the Deployment Page's rows reuse it. */
export function compareNodeSettings(configs: ReturnType<typeof parseNodeConfigs>): ServiceSettingChange[] {
  const settings = configs.type === "service" ? compareServiceSettings(configs.current, configs.baseline)
    : compareResourceSettings("volume", configs.current, configs.baseline);
  return settings.filter((row) => row.path !== "node");
}
function compare(baseline: ComparedState, working: ComparedState, intro: ReadonlyMap<string, ComparedState["nodes"][number]>) {
  const before = nodeMap(baseline); const after = nodeMap(working); const groups: DashboardReviewChangeSet["groups"] = [];
  for (const key of [...new Set([...before.keys(), ...after.keys()])].sort()) {
    const previous = before.get(key)?.config ?? null; const next = after.get(key)?.config ?? null; const entry = after.get(key) ?? before.get(key);
    if (!entry) continue;
    const baseline = previous ?? intro.get(key)?.config ?? null;
    const settings = next && baseline ? compareNodeSettings(parseNodeConfigs(entry.node.type, next, baseline)) : [];
    const lifecycle = !previous && next ? "create" : previous && !next ? "delete" : previous && settings.length ? "update" : null;
    if (lifecycle) groups.push({ node: entry.node, lifecycle, settings,
      comparison: previous ? "head" : intro.get(key)?.config ? "introduction" : null });
  }
  return groups;
}
export function buildEnvironmentChangeSet(input: EnvironmentChangeSetProjectionInput): DashboardReviewChangeSet {
  const head = input.submitted ?? input.applied;
  const introductions = nodeMap(input.nodeIntroductions);
  for (const entry of [...input.applied.nodes, ...input.saved?.nodes ?? []]) {
    if (entry.config) introductions.delete(`${entry.node.type}:${entry.node.id}`);
  }
  const groups = compare(head, input.working, introductions);
  return {
    groups,
    totalCount: groups.reduce((n, group) => n + group.settings.length + (group.lifecycle === "update" ? 0 : 1), 0),
    headToken: head.token,
  };
}

/** One node's config as Working or Applied State holds it. */
export type StateNode = { nodeType: EnvironmentNodeIdentity["type"]; nodeId: string; config: NodeConfig };

/**
 * Whether Working State differs from Applied State, by the rule `buildEnvironmentChangeSet` counts with. A Branch must
 * have none before it merges or updates; its review page shows them as staged.
 */
export function hasUndeployedChanges(working: readonly StateNode[], applied: readonly StateNode[]): boolean {
  const state = (nodes: readonly StateNode[]) => ({ nodes: nodes.map((node) => ({ node: { type: node.nodeType, id: node.nodeId }, config: node.config })) });
  return compare(state(applied), state(working), new Map()).length > 0;
}

/** The change group for one node, computed by the same rule as the whole set. */
export function buildEnvironmentNodeChange(input: {
  working: EnvironmentNodeProjection;
  applied: EnvironmentNodeProjection[];
  saved: EnvironmentNodeProjection[] | null;
  submitted: EnvironmentNodeProjection[] | null;
  introduction: EnvironmentNodeIntroductionProjection | null;
}): DashboardReviewNodeChange | null {
  const only = (nodes: EnvironmentNodeProjection[]) =>
    nodes.filter(node => node.node.type === input.working.node.type && node.node.id === input.working.node.id);
  return buildEnvironmentChangeSet({
    working: { token: "working", nodes: [input.working] },
    applied: { token: "applied", nodes: only(input.applied) },
    saved: input.saved ? { token: "saved", nodes: only(input.saved) } : null,
    submitted: input.submitted ? { token: "submitted", nodes: only(input.submitted) } : null,
    nodeIntroductions: { token: "introductions", nodes: input.introduction ? [input.introduction] : [] },
  }).groups[0] ?? null;
}
