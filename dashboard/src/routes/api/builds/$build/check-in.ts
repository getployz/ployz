import { createFileRoute } from "@tanstack/react-router";
import { checkInStoreGithubBuild } from "#/modules/config-store/store-github-builds.server";
import { buildCall } from "#/routes/api/builds/-build-call";
import { apiRoute } from "#/server/api-route";

/** A GitHub runner's one check-in, authenticated by its OIDC token. */
export const Route = createFileRoute("/api/builds/$build/check-in")({
  server: {
    handlers: apiRoute<{ build: string }>({
      // A build's id is DEPLOYMENT.SERVICE.
      POST: ({ request, params }) => buildCall(request, (text) => checkInStoreGithubBuild(request, params.build, text)),
    }),
  },
});
