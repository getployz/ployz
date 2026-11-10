import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { grantRuleBuild } from "#/modules/config-store/store-github-builds.server";
import { publicErrorResponse, methodNotAllowed } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";

/** A rule build's grant ask, relayed to a Machine that checks it. Authenticated by the runner's OIDC token. */
export const Route = createFileRoute("/api/builds/$build/grant")({
  server: {
    handlers: {
      POST: async ({ request, params }) => {
        const text = await request.text();
        const options = { signal: request.signal };
        return runAppEffect(grantRuleBuild(request, params.build, text).pipe(Effect.map((body) => Response.json(body))), options)
          .catch(publicErrorResponse);
      },
      ANY: (): Response => methodNotAllowed(Route),
    },
  },
});
