import type { ServiceConfig } from "@ployz/sdk/config";

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
