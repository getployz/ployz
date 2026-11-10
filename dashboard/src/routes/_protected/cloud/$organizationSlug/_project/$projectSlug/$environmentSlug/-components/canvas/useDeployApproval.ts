import { useLoaderData } from "@tanstack/react-router";
import type { EnvironmentRef, Signed } from "@ployz/sdk";
import { deployApprovalScope } from "#/modules/approval-session/approval-prompt";
import { type ApprovalScope, approveDeploy, SessionRefused } from "#/modules/approval-session/approval-session";
import { draftDeployApprovalServerFn } from "#/modules/approval-session/deploy-approval.functions";
import { namespaceQuery, useCachedStoreView } from "#/modules/config-store/store-view.queries";
import { StoreRefused } from "#/modules/config-store/store.contract";
import { useRuntimeLens } from "#/modules/runtime/use-runtime-lens";
import { useEnvironmentMachines } from "#/providers/runtime-provider";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";

const UNCHECKED = "Cloud couldn't check the servers. Try again.";

/**
 * A Deploy's approval on Servers that enforce. `scope` runs at the press: undefined when no Server enforces, so the
 * Deploy goes as before; null when the person cancels the prompt. `approve` runs in the write queue, after the edits
 * before it, and signs the plan at the version the person reviewed; a refusal reads as the Store's.
 */
export function useDeployApproval(organizationSlug: string, environment: EnvironmentRef) {
  const { environmentId } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const view = useCachedStoreView(organizationSlug, namespaceQuery(environment));
  const namespace = view?.ok ? view.value.namespace : null;
  const { machines } = useRuntimeLens(organizationSlug);
  const environmentMachines = useEnvironmentMachines(namespace);

  async function scope(): Promise<ApprovalScope | null | undefined> {
    if (namespace !== null) {
      return deployApprovalScope({ environment: { environmentId, environmentName: environment.environment ?? "", namespace }, environmentMachines, machines });
    }
    if (machines.some((machine) => machine.deviceEnforcement === true)) throw new Error(UNCHECKED);
    return undefined;
  }

  async function approve(scope: ApprovalScope, deployment: string, version: string): Promise<Signed> {
    try {
      return await approveDeploy(scope, deployment, async (terms) => {
        const drafted = await draftDeployApprovalServerFn({ data: { organizationSlug, environment, version, terms } });
        if (!drafted.ok) throw new StoreRefused(drafted.refusal);
        return drafted.body;
      });
    } catch (error) {
      if (error instanceof StoreRefused) throw error;
      const message = error instanceof SessionRefused ? error.message : "This browser couldn't approve the deploy. Try again.";
      throw new StoreRefused({ code: "unapproved", message, details: null });
    }
  }

  return { scope, approve };
}
