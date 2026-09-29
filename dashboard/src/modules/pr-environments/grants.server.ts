import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { fetchAppInstallationPermissions } from "#/modules/github/github-observation.api";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import { environment } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { callStoreAsMember } from "#/modules/config-store/config-store.server";
import { planRepositories } from "./repositories";

/** An installation that hasn't accepted what PR Environments need: Pull requests: read and Checks: write. */
export type PrEnvironmentGrantMissing = { installationId: number; url: string };

/** Whether an installation lacks what PR Environments need, and where its owner approves it. */
const missingGrant = (installationId: number) => fetchAppInstallationPermissions(installationId).pipe(
  Effect.map(({ url, permissions: { pull_requests: pullRequests, checks } }): PrEnvironmentGrantMissing | null => {
    const ready = (pullRequests === "read" || pullRequests === "write") && checks === "write";
    return ready ? null : { installationId, url };
  }),
);

/**
 * The GitHub App installations the organization's services deploy through that PR Environments can't use yet, as GitHub
 * reports their granted permissions.
 */
export const listMissingPrEnvironmentGrants = Effect.fn("PrEnvironments.listMissingGrants")(
  function* (actor: Actor, input: { organizationSlug: string }) {
    const organization = yield* getOrganizationForUserBySlug(actor.userId, input.organizationSlug);
    if (!organization) return yield* new NotFound({ message: "Organization not found." });
    const { drizzle } = yield* Database;
    const environments = yield* drizzle.select({ intent: environment.intent }).from(environment)
      .where(eq(environment.organizationId, organization.id));
    const installationIds = [...new Set(planRepositories(environments).map((repository) => repository.installationId))];
    const checked = yield* Effect.forEach(installationIds, missingGrant, { concurrency: 4 });
    return checked.filter((missing) => missing !== null);
  },
);

/** The same over the Config Store, for one Project: the installations its PR plans' repositories deploy through. */
export const listMissingStorePrGrants = Effect.fn("PrEnvironments.listMissingStoreGrants")(
  function* (actor: Actor, input: { organizationSlug: string; projectSlug: string }) {
    const result = yield* callStoreAsMember(actor, input.organizationSlug, { operation: "read", query: { query: "pr_plans", project: input.projectSlug } });
    const view = result.ok && "view" in result.value && result.value.view === "pr_plans" ? result.value : null;
    const installationIds = [...new Set(view?.plans.map((plan) => plan.installation_id))];
    const checked = yield* Effect.forEach(installationIds, missingGrant, { concurrency: 4 });
    return checked.filter((missing) => missing !== null);
  },
);
