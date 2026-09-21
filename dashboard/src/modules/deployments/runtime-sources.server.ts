import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Database } from "#/server/database.server";
import { materializeGithubSource, resolveGithubSourceSha, GithubSourceError } from "#/modules/github/github-source.server";
import type { DeploymentContext } from "./runtime-repository.contract";
import { persistDeploymentSourcePin } from "./source-pins.server";

export const acquireDeploymentSources = Effect.fn("Deployments.acquireSources")(function* (
  context: DeploymentContext,
  onSource: (serviceId: string) => Effect.Effect<void, Error, Database>,
) {
  const sources: Record<string, string> = {};
  for (const snapshot of context.snapshots) {
    const source = snapshot.config.source;
    if (source.type !== "git") continue;
    yield* onSource(snapshot.serviceId);
    const identity = { organizationId: context.organization.id, installationId: source.installationId, repositoryId: source.repositoryId };
    let sha = context.deployment.sourcePins?.[snapshot.serviceId]?.commitSha;
    if (!sha) {
      if (source.branch.type !== "connected") return yield* new GithubSourceError({ message: "Reconnect the source branch before deploying." });
      if (!context.deployment.inngestRunId) return yield* new GithubSourceError({ message: "Source acquisition requires an owned deployment attempt." });
      sha = yield* resolveGithubSourceSha({ ...identity, branch: source.branch.name });
      yield* persistDeploymentSourcePin({ ...identity, environmentDeploymentId: context.deployment.id, inngestRunId: context.deployment.inngestRunId, serviceId: snapshot.serviceId, commitSha: sha });
    }
    const capture: Parameters<typeof materializeGithubSource>[0] = { ...identity, sha, rootDir: source.rootDir };
    if (snapshot.config.build.builder === "dockerfile") capture.dockerfilePath = snapshot.config.build.dockerfilePath ?? "Dockerfile";
    const checkout = yield* materializeGithubSource(capture);
    sources[snapshot.config.privateDns] = checkout.repositoryDirectory;
  }
  return sources;
});
