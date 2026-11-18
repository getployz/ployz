import { useLoaderData } from "@tanstack/react-router";
import type { EnvironmentRef } from "@ployz/sdk";
import { useCollectionScope } from "#/collections/use-collection-scope";
import { deployApprovalScope } from "#/modules/approval-session/approval-prompt";
import { type ApprovalScope, approveDeploy, type DeployApproved, SessionRefused } from "#/modules/approval-session/approval-session";
import { deployPlanQuery, namespaceQuery, storeViewOptions, useCachedStoreView } from "#/modules/config-store/store-view.queries";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { environmentMachineIds, getRuntimeCollections, settledRuntimeSnapshot } from "#/modules/runtime/runtime.collection";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

const UNCHECKED = "Cloud couldn't check the servers. Try again.";

/**
 * A Deploy's approval. `scope` runs at the press, and waits for Cloud to list the Servers: undefined when Cloud knows
 * there is no Server, so nothing would run it; null when the person cancels the prompt. It throws when Cloud can't
 * list them, since a Deploy no passkey approved is one the Servers may refuse. `approve` runs in the write queue, after the edits
 * before it: it reads the plan, and this browser builds and signs the approval of it at the version the person
 * reviewed. A refusal reads as the Store's.
 */
export function useDeployApproval(organizationSlug: string, environment: EnvironmentRef) {
  const collectionScope = useCollectionScope();
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const view = useCachedStoreView(organizationSlug, namespaceQuery(environment));
  const namespace = view?.ok ? view.value.namespace : null;
  const runtime = getRuntimeCollections(organizationSlug, collectionScope);

  async function scope(): Promise<ApprovalScope | null | undefined> {
    const seen = await settledRuntimeSnapshot(runtime);
    if (seen === null) throw new Error(UNCHECKED);
    const { machines, services, status } = seen;
    if (machines.length === 0 && (status === "no_connection" || status === "observed")) return undefined;
    if (machines.length === 0 || namespace === null) throw new Error(UNCHECKED);
    const environmentMachines = environmentMachineIds(services, namespace);
    return deployApprovalScope({ environment: { environmentId, environmentName: environment.environment ?? "", namespace }, environmentMachines, machines });
  }

  async function approve(scope: ApprovalScope, deployment: string, version: string): Promise<DeployApproved> {
    const plan = await collectionScope.queryClient.fetchQuery({ ...storeViewOptions(organizationSlug, collectionScope, deployPlanQuery(environment)), staleTime: 0 });
    if (!plan.ok) throw new StoreRefused(plan.refusal);
    try {
      return await approveDeploy(scope, deployment, version, plan.value);
    } catch (error) {
      const message = error instanceof SessionRefused ? error.message : "This browser couldn't approve the deploy. Try again.";
      throw new StoreRefused({ code: "unapproved", message, details: null });
    }
  }

  return { scope, approve };
}
