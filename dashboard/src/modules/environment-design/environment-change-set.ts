import { projectEnvironmentChanges, type ReviewChangeSet } from "@ployz/sdk/config";
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

export function buildEnvironmentChangeSet(input: EnvironmentChangeSetProjectionInput): ReviewChangeSet {
  return projectEnvironmentChanges(input);
}
