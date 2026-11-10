import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { checkInRuleBuild } from "#/modules/config-store/store-github-builds.server";
import { publicErrorResponse, methodNotAllowed } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** A rule build's check-in, on the run its push started, authenticated by its OIDC token. */
export const Route = createFileRoute("/api/builds/check-in")({
  server: {
    handlers: {
      POST: async ({ request }) => {
        const text = await request.text();
        const options = { signal: request.signal };
        return runAppEffect(checkInRuleBuild(request, text).pipe(Effect.map((body) => Response.json(body))), options)
          .catch(publicErrorResponse);
      },
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
