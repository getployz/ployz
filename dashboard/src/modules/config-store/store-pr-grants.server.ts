import "@tanstack/react-start/server-only";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { fetchAppInstallationPermissions } from "#/modules/github/github-observation.api";
import { callStoreAsMember } from "./config-store.server";

/** An installation that hasn't accepted what PR Environments need: Pull requests: read and Checks: write. */
export type PrEnvironmentGrantMissing = { installationId: number; url: string };

/** Whether an installation lacks what PR Environments need, and where its owner approves it. */
const missingGrant = (installationId: number) => fetchAppInstallationPermissions(installationId).pipe(
  Effect.map(({ url, permissions: { pull_requests: pullRequests, checks } }): PrEnvironmentGrantMissing | null => {
    const ready = (pullRequests === "read" || pullRequests === "write") && checks === "write";
    return ready ? null : { installationId, url };
  }),
);

/** The GitHub App installations one Project's PR plans deploy through that PR Environments can't use yet. */
export const listMissingStorePrGrants = Effect.fn("PrEnvironments.listMissingStoreGrants")(
  function* (actor: Actor, input: { organizationSlug: string; projectSlug: string }) {
    const result = yield* callStoreAsMember(actor, input.organizationSlug, { operation: "read", query: { query: "pr_plans", project: input.projectSlug } });
    const view = result.ok && "view" in result.value && result.value.view === "pr_plans" ? result.value : null;
    const installationIds = [...new Set(view?.plans.map((plan) => plan.installation_id))];
    // GitHub not answering hides the warning rather than the page; the next read asks again.
    const checked = yield* Effect.forEach(installationIds, (installationId) => missingGrant(installationId).pipe(
      Effect.catchTag("GithubObservationError", () => Effect.succeed(null)),
    ), { concurrency: 4 });
    return checked.filter((missing) => missing !== null);
  },
);
