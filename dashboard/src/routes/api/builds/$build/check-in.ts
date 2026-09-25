import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { checkInGithubBuild } from "#/modules/deployments/github-image-builds.server";
import { publicErrorResponse } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** A GitHub runner's one check-in, authenticated by its OIDC token. */
export const Route = createFileRoute("/api/builds/$build/check-in")({
  server: {
    handlers: {
      POST: async ({ request, params }) =>
        runAppEffect(checkInGithubBuild(request, params.build).pipe(Effect.map((body) => Response.json(body))), {
          signal: request.signal,
        }).catch(publicErrorResponse),
    },
  },
});
