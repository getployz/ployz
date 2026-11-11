import type { DeploymentView, EnvironmentRef, Signed } from "@ployz/sdk";
import { useLiveQuery } from "@tanstack/react-db";
import { toast } from "sonner";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { toErrorMessage } from "#/lib/error-message";
import { reviewApproval } from "#/modules/approval-session/approval-prompt";
import { getRuntimeCollections, projectRuntimeServiceRecord } from "#/modules/runtime/runtime.collection";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { type Again, approveAgain, approveEnvironmentDeploy, approveRedeploy, type Approving, BUILD_LOOK_MS, type Reads, type Servers } from "./deploy-approvals";
import { deploymentQuery, deployPlanQuery, fetchStoreView, namespaceQuery, redeployPlanQuery, ruleQuery } from "./store-view.queries";
import { useStoreWriter } from "./store-write";

/**
 * The approvals of the deploys the dashboard starts outside its Deploy button, on Servers that enforce Device approval
 * (`deploy-approvals.ts`). Each reads the Servers at the press: the Cluster those running its Environment hold the key
 * of is the one the approval names.
 */
export function useDeployApprovals(organizationSlug: string) {
  const scope = useCollectionScope();
  const writer = useStoreWriter(organizationSlug);
  const { machines } = useRuntimeLens(organizationSlug);
  const { data: services = [] } = useLiveQuery(getRuntimeCollections(organizationSlug, scope).services);
  const servers: Servers = {
    machines,
    running: (namespace) => [...new Set(services.filter((row) => row.identity.startsWith(`${namespace}/`)).map(projectRuntimeServiceRecord)
      .flatMap((service) => [...service.containers, ...service.hookContainers]).map((copy) => copy.machineId))],
  };
  const reads: Reads = {
    plan: (environment, deployment) => fetchStoreView(organizationSlug, scope,
      deployment === undefined ? deployPlanQuery(environment) : redeployPlanQuery(environment, deployment)),
    namespace: async (environment) => (await fetchStoreView(organizationSlug, scope, namespaceQuery(environment))).namespace,
    rule: (environment) => fetchStoreView(organizationSlug, scope, ruleQuery(environment)),
    deployment: (id) => fetchStoreView(organizationSlug, scope, deploymentQuery(id)),
  };

  const approving: Approving = { reads, servers, review: reviewApproval };

  return {
    /** A Deploy of `environment`; one that runs nowhere yet places where `near`, its Parent, runs. */
    deploy: (environment: EnvironmentRef, id: string, near: EnvironmentRef) => approveEnvironmentDeploy(approving, environment, id, near),
    redeploy: (source: DeploymentView, id: string) => approveRedeploy(approving, source, id),
    /** Approves redeploy `id`'s commits again once its builds end, in the background; a failure toasts and cancels it. */
    follow(id: string, again: Again) {
      void approveAgain({
        read: () => reads.deployment(id),
        approve: (approvals: Signed[]) => writer.commit({ command: "approve_deployment", deployment: id, approvals }),
        cancel: () => writer.commit({ command: "cancel", deployment: id }),
        wait: () => new Promise((resolve) => setTimeout(resolve, BUILD_LOOK_MS)),
      }, again).catch((error: Error) => toast.error(toErrorMessage(error, "Couldn't approve the redeploy")));
    },
  };
}
