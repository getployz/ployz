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
export type EnvironmentChangeSetProjectionInput = {
  working: EnvironmentStateProjection;
  saved: EnvironmentStateProjection;
  applied: EnvironmentStateProjection;
  submitted: EnvironmentStateProjection | null;
  nodeIntroductions: EnvironmentNodeIntroductionsProjection;
};

export type EnvironmentWorkingComparison<T> = { role: "baseline" | "node_introduction"; value: T } | null;
export function resolveEnvironmentWorkingComparison<T>(input: {
  baseline: T | null; introduction: T | null;
}): EnvironmentWorkingComparison<T> {
  return input.baseline ? { role: "baseline", value: input.baseline }
    : input.introduction ? { role: "node_introduction", value: input.introduction } : null;
}

export type DashboardReviewChangeSet = { groups: Array<{ node: EnvironmentNodeIdentity; lifecycle: "create" | "update" | "delete"; settings: ServiceSettingChange[] }>; totalCount: number; canSave: boolean };

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
    if (lifecycle) groups.push({ node: entry.node, lifecycle, settings });
  }
  return groups;
}
export function buildEnvironmentChangeSet(input: EnvironmentChangeSetProjectionInput): DashboardReviewChangeSet {
  const baseline = input.submitted ?? input.applied;
  const saved = nodeMap(input.saved);
  const applied = nodeMap(input.applied);
  const introductions = { ...input.nodeIntroductions, nodes: input.nodeIntroductions.nodes.filter((entry) => {
    const key = `${entry.node.type}:${entry.node.id}`;
    return saved.get(key)?.config == null && applied.get(key)?.config == null;
  }) };
  const groups = compare(baseline, input.working, introductions);
  const saveGroups = compare(input.saved, input.working, { token: "", nodes: [] });
  return { groups, totalCount: groups.reduce((n, group) => n + group.settings.length + (group.lifecycle === "update" ? 0 : 1), 0), canSave: saveGroups.length > 0 };
}
