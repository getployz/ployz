import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect, Schema } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { GithubApi } from "#/modules/github/github-observation.api";
import { githubIdSchema } from "#/modules/github/github-ingestion.contracts";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import { environment } from "#/modules/project/tables";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";
import { planRepositories } from "./repositories";

const installationSchema = Schema.Struct({
  id: githubIdSchema,
  html_url: Schema.String,
  permissions: Schema.Record(Schema.String, Schema.String),
});

/** An installation that hasn't accepted what PR Environments need: Pull requests: read and Checks: write. */
export type PrEnvironmentGrantMissing = { installationId: number; url: string };

/**
 * The GitHub App installations the organization's services deploy through that PR Environments can't use yet, as GitHub
 * reports their granted permissions. An installation GitHub can't answer for is left out: nothing to say about it.
 */
export const listMissingPrEnvironmentGrants = Effect.fn("PrEnvironments.listMissingGrants")(
  function* (actor: Actor, input: { organizationSlug: string }) {
    const organization = yield* getOrganizationForUserBySlug(actor.userId, input.organizationSlug);
    if (!organization) return yield* new NotFound({ message: "Organization not found." });
    const { drizzle } = yield* Database;
    const environments = yield* drizzle.select({ intent: environment.intent }).from(environment)
      .where(eq(environment.organizationId, organization.id));
    const installationIds = [...new Set(planRepositories(environments).map((repository) => repository.installationId))];
    const api = yield* GithubApi;
    const checked = yield* Effect.forEach(installationIds, (installationId) => api.json({
      auth: "app",
      installationId,
      url: `https://api.github.com/app/installations/${installationId}`,
      operation: "fetch_installation",
      schema: installationSchema,
    }).pipe(
      Effect.map((installation): PrEnvironmentGrantMissing | null => {
        const { pull_requests: pullRequests, checks } = installation.permissions;
        const ready = (pullRequests === "read" || pullRequests === "write") && checks === "write";
        return ready ? null : { installationId, url: installation.html_url };
      }),
      Effect.catch((error) => Effect.logWarning("GitHub installation permissions unavailable", { installationId, error }).pipe(Effect.as(null))),
    ), { concurrency: 4 });
    return checked.filter((missing) => missing !== null);
  },
);
