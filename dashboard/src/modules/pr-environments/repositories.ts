import { planBranch, type BranchPicks, type ServiceConfig } from "@ployz/sdk/config";
import { branchSetupCommands, copiesData, listNames } from "#/modules/branches/branch-plan";

export type PlanRepository = { installationId: number; repositoryId: number; repository: string };

/** The repository a service deploys from through the GitHub App, or null. */
export function githubAppRepository(config: Pick<ServiceConfig, "source">): PlanRepository | null {
  const { source } = config;
  if (source.type !== "git" || source.access.type !== "github-installation") return null;
  return { installationId: source.access.installationId, repositoryId: source.repositoryId, repository: source.repository };
}

/** Every GitHub repository these Environments' services deploy from through the GitHub App, once each, by name. */
export function planRepositories(environments: ReadonlyArray<{ intent: { services: ReadonlyArray<{ config: Pick<ServiceConfig, "source"> }> } }>) {
  const byId = new Map<number, PlanRepository>();
  for (const environment of environments) {
    for (const service of environment.intent.services) {
      const repository = githubAppRepository(service.config);
      if (repository && !byId.has(repository.repositoryId)) byId.set(repository.repositoryId, repository);
    }
  }
  return [...byId.values()].sort((a, b) => a.repository.localeCompare(b.repository));
}

/**
 * What core plans for a repository's PR Environments over its start-from Environment's Working State (`parent`): the
 * repository's services are always Own Copies, and hand picks are kept by lineage, dropping any the Environment lacks.
 */
export function prPlanInput(parent: Parameters<typeof planBranch>[0]["parent"], deployed: string[], repositoryId: number, picks: BranchPicks) {
  const focus = parent.services.filter((node) => githubAppRepository(node.config)?.repositoryId === repositoryId).map((node) => node.lineageId);
  const owned = new Set([...parent.services.map((node) => node.lineageId), ...parent.volumes.map((node) => node.resourceLineageId)]);
  return {
    parent, deployed, focus,
    picks: "own" in picks ? { own: [...new Set([...focus, ...picks.own.filter((lineage) => owned.has(lineage))])] } : picks,
  };
}

/**
 * What a PR Environment gets, in words: "On · from staging · empty data · then php artisan migrate · postgres used live".
 * `input` is the plan over its start-from Environment (`prPlanInput`), null while that loads.
 */
export function planSummary(
  plan: { enabled: boolean; setupCommands: ReadonlyArray<{ lineageId: string; command: string }> },
  startFrom: string | undefined,
  input: ReturnType<typeof prPlanInput> | null,
  nameOf: (lineage: string) => string,
) {
  if (!plan.enabled) return "Off";
  if (!startFrom) return "On · pick an environment to start from";
  const parts = ["On", `from ${startFrom}`];
  if (input) {
    const branch = planBranch(input);
    const focus = new Set(input.focus);
    const names = (keep: (node: (typeof branch.nodes)[number]) => boolean) => branch.nodes.filter(keep).map((node) => nameOf(node.lineageId));
    const copied = names((node) => node.role === "own" && !focus.has(node.lineageId));
    const live = names((node) => node.role === "live");
    if (copied.length) parts.push(branch.nodes.every((node) => node.role === "own") ? "everything copied" : `${listNames(copied)} copied`);
    if (copiesData(branch)) parts.push("empty data");
    const commands = branchSetupCommands(branch, plan.setupCommands);
    if (commands.length) parts.push(`then ${commands.map((setup) => setup.command).join(", ")}`);
    if (live.length) parts.push(live.length > 2 ? `the rest used live from ${startFrom}` : `${listNames(live)} used live`);
  }
  return parts.join(" · ");
}
