import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { recordGithubBuildSteps } from "#/modules/deployments/github-image-builds.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** A GitHub runner's Build Steps, authenticated by its OIDC token. */
export const Route = createFileRoute("/api/builds/$build/steps")({
  server: {
    handlers: {
      POST: async ({ request, params }) =>
        runAppEffect(recordGithubBuildSteps(request, params.build, await request.text()).pipe(Effect.map((body) => Response.json(body))), {
          signal: request.signal,
        }).catch(publicErrorResponse),
    },
  },
});
