import type { JsonObject } from "#/db/tables";
import { compareResourceSettings, compareServiceSettings, parseResourceConfig, parseServiceConfig, projectEnvironmentChanges,
  type ChangeSetInput, type ReviewChangeSet, type ReviewNodeChange, type ServiceSettingChange } from "@ployz/sdk/config";
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

type NodeConfig = NonNullable<EnvironmentNodeProjection["config"]> | JsonObject;
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
/** Core's Environment Change Set. */
export function buildEnvironmentChangeSet(input: EnvironmentChangeSetProjectionInput): ReviewChangeSet {
  return projectEnvironmentChanges(input);
}

/** One node's config as Working or Applied State holds it. */
export type StateNode = { nodeType: EnvironmentNodeIdentity["type"]; nodeId: string; config: NodeConfig };

/**
 * Whether Working State differs from Applied State, by the rule `buildEnvironmentChangeSet` counts with. A Branch must
 * have none before it merges or updates; its review page shows them as staged.
 */
export function hasUndeployedChanges(working: readonly StateNode[], applied: readonly StateNode[]): boolean {
  const state = (token: string, nodes: readonly StateNode[]) =>
    ({ token, nodes: nodes.map((node) => ({ node: { type: node.nodeType, id: node.nodeId }, config: node.config })) });
  const input = { working: state("working", working), applied: state("applied", applied), saved: null, submitted: null,
    nodeIntroductions: state("introductions", []) };
  // SAFETY: Core parses every config it compares and rejects one that isn't a node config.
  return projectEnvironmentChanges(input as ChangeSetInput).groups.length > 0;
}

/** The change group for one node, computed by the same rule as the whole set. */
export function buildEnvironmentNodeChange(input: {
  working: EnvironmentNodeProjection;
  applied: EnvironmentNodeProjection[];
  saved: EnvironmentNodeProjection[] | null;
  submitted: EnvironmentNodeProjection[] | null;
  introduction: EnvironmentNodeIntroductionProjection | null;
}): ReviewNodeChange | null {
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
