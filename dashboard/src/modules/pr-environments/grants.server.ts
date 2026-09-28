import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { fetchAppInstallationPermissions } from "#/modules/github/github-observation.api";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import { environment } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { planRepositories } from "./repositories";

/** An installation that hasn't accepted what PR Environments need: Pull requests: read and Checks: write. */
export type PrEnvironmentGrantMissing = { installationId: number; url: string };

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
    const checked = yield* Effect.forEach(installationIds, (installationId) => fetchAppInstallationPermissions(installationId).pipe(
      Effect.map(({ url, permissions: { pull_requests: pullRequests, checks } }): PrEnvironmentGrantMissing | null => {
        const ready = (pullRequests === "read" || pullRequests === "write") && checks === "write";
        return ready ? null : { installationId, url };
      }),
    ), { concurrency: 4 });
    return checked.filter((missing) => missing !== null);
  },
);
