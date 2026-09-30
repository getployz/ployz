import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { checkInStoreGithubBuild } from "#/modules/config-store/store-github-builds.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** A GitHub runner's one check-in, authenticated by its OIDC token. */
export const Route = createFileRoute("/api/builds/$build/check-in")({
  server: {
    handlers: {
      POST: async ({ request, params }) => {
        const options = { signal: request.signal };
        // A build's id is DEPLOYMENT.SERVICE.
        return runAppEffect(checkInStoreGithubBuild(request, params.build).pipe(Effect.map((body) => Response.json(body))), options)
          .catch(publicErrorResponse);
      },
    },
  },
});
