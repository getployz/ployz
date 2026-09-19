import { parseDashboardServiceConfig } from "./service-config";
import { compareResourceSettings, parseResourceConfig, type ServiceSettingChange } from "@ployz/sdk/config";
import { compareDashboardServiceSettings, compareVariableGroupSettings } from "./config-changes";
import type { EnvironmentResourceNodeConfigByType } from "./environment-resource-node";
import type { ServiceDeploymentConfig } from "./services";

export type EnvironmentNodeLifecycle = "create" | "update" | "delete" | "none";
export type EnvironmentNodeIdentity = { type: "service" | "variable_group" | "volume"; id: string };
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
/** Head is `submitted ?? applied`; a node absent from Head compares against its Introduction. */
export type EnvironmentChangeSetProjectionInput = {
  working: EnvironmentStateProjection;
  applied: EnvironmentStateProjection;
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

function nodeMap(state: EnvironmentStateProjection) { return new Map(state.nodes.map((entry) => [`${entry.node.type}:${entry.node.id}`, entry])); }
function changes(type: EnvironmentNodeIdentity["type"], current: EnvironmentNodeProjection["config"], baseline: EnvironmentNodeProjection["config"]): ServiceSettingChange[] {
  if (!current || !baseline) return [];
  if (type === "service") return compareDashboardServiceSettings(parseDashboardServiceConfig(current), parseDashboardServiceConfig(baseline));
  if (type === "volume") return compareResourceSettings("volume", parseResourceConfig("volume", current), parseResourceConfig("volume", baseline));
  return compareVariableGroupSettings(current, baseline);
}
function compare(baseline: EnvironmentStateProjection, working: EnvironmentStateProjection, introductions: EnvironmentStateProjection) {
  const before = nodeMap(baseline); const after = nodeMap(working); const intro = nodeMap(introductions); const groups: DashboardReviewChangeSet["groups"] = [];
  for (const key of [...new Set([...before.keys(), ...after.keys()])].sort()) {
    const previous = before.get(key)?.config ?? null; const next = after.get(key)?.config ?? null; const entry = after.get(key) ?? before.get(key);
    if (!entry) continue;
    const settings = changes(entry.node.type, next, previous ?? intro.get(key)?.config ?? null).filter((row) => row.path !== "node" && !("derivedFrom" in row && row.derivedFrom));
    const lifecycle = !previous && next ? "create" : previous && !next ? "delete" : previous && settings.length ? "update" : null;
    if (lifecycle) groups.push({ node: entry.node, lifecycle, settings,
      comparison: previous ? "head" : intro.get(key)?.config ? "introduction" : null });
  }
  return groups;
}
export function buildEnvironmentChangeSet(input: EnvironmentChangeSetProjectionInput): DashboardReviewChangeSet {
  const head = input.submitted ?? input.applied;
  const groups = compare(head, input.working, input.nodeIntroductions);
  return {
    groups,
    totalCount: groups.reduce((n, group) => n + group.settings.length + (group.lifecycle === "update" ? 0 : 1), 0),
    headToken: head.token,
  };
}

/** The change group for one node, computed by the same rule as the whole set. */
export function buildEnvironmentNodeChange(input: {
  working: EnvironmentNodeProjection;
  applied: EnvironmentNodeProjection[];
  submitted: EnvironmentNodeProjection[] | null;
  introduction: EnvironmentNodeIntroductionProjection | null;
}): DashboardReviewNodeChange | null {
  const only = (nodes: EnvironmentNodeProjection[]) =>
    nodes.filter(node => node.node.type === input.working.node.type && node.node.id === input.working.node.id);
  return buildEnvironmentChangeSet({
    working: { token: "working", nodes: [input.working] },
    applied: { token: "applied", nodes: only(input.applied) },
    submitted: input.submitted ? { token: "submitted", nodes: only(input.submitted) } : null,
    nodeIntroductions: { token: "introductions", nodes: input.introduction ? [input.introduction] : [] },
  }).groups[0] ?? null;
}
