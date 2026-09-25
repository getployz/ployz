import { queryOptions, skipToken, useQuery } from "@tanstack/react-query";
import { listDeploymentBuildLogServerFn, listDeploymentBuildTailServerFn } from "./deployment.functions";

type BuildLogPage = Awaited<ReturnType<typeof listDeploymentBuildLogServerFn>>;
export type BuildStepRow = BuildLogPage["steps"][number];
export type BuildOutputRow = BuildLogPage["output"][number];

type BuildLog = { steps: BuildStepRow[]; output: BuildOutputRow[]; imageBuilds: BuildLogPage["imageBuilds"]; finished: boolean };

/** Polls the step tree while the attempt runs, resuming output from the last row already held. */
export function deploymentBuildLogQueryOptions(organizationSlug: string, deploymentId: string) {
  const queryKey = ["deployment-build-log", organizationSlug, deploymentId];
  return queryOptions<BuildLog>({
    queryKey,
    // Each mount resumes from the last held row; a running build also polls.
    staleTime: 0,
    refetchInterval: (query) => query.state.data?.finished ? false : 2_000,
    queryFn: async ({ signal, client }) => {
      const previous = client.getQueryData<BuildLog>(queryKey);
      let steps: BuildStepRow[] = [];
      let imageBuilds: BuildLogPage["imageBuilds"] = [];
      const output: BuildOutputRow[] = [...previous?.output ?? []];
      let finished = false;
      const last = output.at(-1);
      let afterSequence: string | null | undefined = last ? String(last.id) : undefined;
      // One page holds almost every log; the loop only continues a longer one.
      while (afterSequence !== null) {
        const page = await listDeploymentBuildLogServerFn({ data: { organizationSlug, deploymentId, afterSequence, limit: 10_000 }, signal });
        steps = page.steps;
        imageBuilds = page.imageBuilds;
        output.push(...page.output);
        finished = page.finished;
        afterSequence = page.nextSequence;
      }
      return { steps, output, imageBuilds, finished };
    },
  });
}

/** Each Build Step's last rows, which is all a node's tail reads; polls while the attempt runs. Null without an attempt. */
export function deploymentBuildTailQueryOptions(organizationSlug: string, deploymentId: string | null) {
  return queryOptions<BuildLogPage | null>({
    queryKey: ["deployment-build-tail", organizationSlug, deploymentId],
    staleTime: 0,
    refetchInterval: (query) => query.state.data?.finished === false ? 2_000 : false,
    queryFn: ({ signal }) => deploymentId === null ? null : listDeploymentBuildTailServerFn({ data: { organizationSlug, deploymentId }, signal }),
  });
}

/** Reads nothing without an attempt. */
export function useBuildLog(organizationSlug: string, deploymentId: string | null) {
  return useQuery(deploymentId === null
    ? { queryKey: ["deployment-build-log", organizationSlug, null], queryFn: skipToken }
    : deploymentBuildLogQueryOptions(organizationSlug, deploymentId));
}

/** The attempt's build tail; `isPending` until it first arrives, so nodes can hold their build stage back. */
export function useBuildTail(organizationSlug: string, deploymentId: string | null) {
  return useQuery(deploymentBuildTailQueryOptions(organizationSlug, deploymentId));
}
