import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Database, ReportingDatabase } from "#/server/database.server";
import { materializeGithubSource, resolveGithubSourceSha, GithubSourceError } from "#/modules/github/github-source.server";
import type { DeploymentContext } from "./runtime-repository.contract";
import { persistDeploymentSourcePin } from "./source-pins.server";

type Snapshot = DeploymentContext["snapshots"][number];
type GitSource = Extract<Snapshot["config"]["source"], { type: "git" }>;

const sourceIdentity = (context: DeploymentContext, source: GitSource) => ({
  organizationId: context.organization.id,
  installationId: source.access.type === "public" ? null : source.access.installationId,
  repositoryId: source.repositoryId,
});

/** The commit this attempt builds for a Git Service: its pin, or the branch head, pinned now. */
export const pinSourceCommit = Effect.fn("Deployments.pinSourceCommit")(function* (context: DeploymentContext, snapshot: Snapshot, source: GitSource) {
  const pinned = context.deployment.sourcePins?.[snapshot.serviceId]?.commitSha;
  if (pinned) return pinned;
  if (source.branch.type !== "connected") return yield* new GithubSourceError({ message: "Reconnect the source branch before deploying." });
  if (!context.deployment.inngestRunId) return yield* new GithubSourceError({ message: "Source acquisition requires an owned deployment attempt." });
  const identity = sourceIdentity(context, source);
  const sha = yield* resolveGithubSourceSha({ ...identity, branch: source.branch.name });
  yield* persistDeploymentSourcePin({ ...identity, environmentDeploymentId: context.deployment.id, inngestRunId: context.deployment.inngestRunId, serviceId: snapshot.serviceId, commitSha: sha });
  return sha;
});

export const acquireDeploymentSources = Effect.fn("Deployments.acquireSources")(function* (
  context: DeploymentContext,
  onSource: (serviceId: string) => Effect.Effect<void, Error, Database | ReportingDatabase>,
) {
  const sources: Record<string, string> = {};
  const source_commits: Record<string, string> = {};
  for (const snapshot of context.snapshots) {
    const source = snapshot.config.source;
    if (source.type !== "git") continue;
    yield* onSource(snapshot.serviceId);
    const sha = yield* pinSourceCommit(context, snapshot, source);
    const capture: Parameters<typeof materializeGithubSource>[0] = { ...sourceIdentity(context, source), sha, rootDir: source.rootDir };
    if (snapshot.config.build.buildMethod === "dockerfile") capture.dockerfilePath = snapshot.config.build.dockerfilePath ?? "Dockerfile";
    const checkout = yield* materializeGithubSource(capture);
    sources[snapshot.config.privateDns] = checkout.repositoryDirectory;
    source_commits[snapshot.config.privateDns] = sha;
  }
  return { sources, source_commits };
});
