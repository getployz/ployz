import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import { REPLANNED_ON_RERUN } from "#/modules/approvals/approvals";
import { getApproval, operationDigest, type OperationDigest, pendingApprovals } from "#/modules/approvals/approvals.server";
import { freshRenameDigest } from "#/modules/cluster-name/cluster-name.server";
import { ownedNamespaces } from "#/modules/config-store/store-namespaces.server";
import { freshSwitchDigest } from "#/modules/ingress/doors.server";
import { drainPlan, ingressOffPlan, observe, ownedOrSystem, planClean } from "#/modules/machines/server-operations.server";
import { withClusterBudget } from "#/modules/runtime/organization-runtime.server";
import type { Database } from "#/server/database.server";

/**
 * An operation approval's preview recomputed on read, so a changed Cluster supersedes it. A Server no longer observed,
 * or a Namespace an Environment owns by now, no longer applies. When the Cluster can't be read within 5 seconds of Cluster
 * time, the stored preview stands.
 */
export const freshOperationDigest: OperationDigest<Effect.Services<ReturnType<typeof planClean>> | Database> = (organizationId, subject, verb, stored, preview) => {
  const separator = subject.indexOf(":");
  const key = subject.slice(separator + 1);
  // A member, invite, Service, Project, Environment, Organization or forget-every-Server approval is planned again when
  // its command reruns; there is no Cluster to recompute it from.
  if (REPLANNED_ON_RERUN.has(subject.slice(0, separator))) return Effect.succeed(stored);
  const fresh = Effect.gen(function* () {
    switch (verb) {
      case "drain": {
        const frame = yield* observe(organizationId);
        const plan = drainPlan(frame, new Set(yield* ownedNamespaces(organizationId)), key);
        return plan === null ? null : operationDigest(plan);
      }
      case "clean":
        if (yield* ownedOrSystem(organizationId, key)) return null;
        return operationDigest(yield* planClean(organizationId, key));
      case "set": {
        const plan = ingressOffPlan(yield* observe(organizationId), key, { acceptsIngress: false });
        return plan === null || plan.effects.length === 0 ? null : operationDigest({ ...plan, preview });
      }
      case "remove":
      case "forget": {
        const frame = yield* observe(organizationId);
        return frame.machines.some(({ machine }) => machine.id === key) ? stored : null;
      }
      case "release":
      case "mirror":
      case "sync":
      case "remove_mirror":
      case "move":
      case "restore":
      case "resize":
      case "stop":
      case "restart":
      case "unprotect":
      case "set_private_access":
        return stored;
      case "set_cluster_name":
        return yield* freshRenameDigest(organizationId, stored, preview);
      case "set_ingress_doors":
        return yield* freshSwitchDigest(organizationId, stored, preview);
    }
  });
  return fresh.pipe(withClusterBudget(5_000), Effect.orElseSucceed(() => stored));
};

/** Every approval still waiting in the Organization, each operation's preview recomputed against the Cluster first. */
export const freshPendingApprovals = (organizationId: string) => pendingApprovals(organizationId, freshOperationDigest);

/** One approval in the Organization, its operation's preview recomputed against the Cluster first. */
export const freshApproval = (organizationId: string, id: string) => getApproval(organizationId, id, freshOperationDigest);
