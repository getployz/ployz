import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { recordStoreGithubBuildSteps } from "#/modules/config-store/store-github-builds.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** A GitHub runner's Build Steps, authenticated by its OIDC token. */
export const Route = createFileRoute("/api/builds/$build/steps")({
  server: {
    handlers: {
      POST: async ({ request, params }) => {
        const text = await request.text();
        const options = { signal: request.signal };
        // A build's id is DEPLOYMENT.SERVICE.
        return runAppEffect(recordStoreGithubBuildSteps(request, params.build, text).pipe(Effect.map((body) => Response.json(body))), options)
          .catch(publicErrorResponse);
      },
    },
  },
});
