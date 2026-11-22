import { createFileRoute } from "@tanstack/react-router";
import { recordStoreGithubBuildSteps } from "#/modules/config-store/store-github-builds.server";
import { buildCall } from "#/routes/api/builds/-build-call";
import { apiRoute } from "#/server/api-route";

/** A GitHub runner's Build Steps, authenticated by its OIDC token. */
export const Route = createFileRoute("/api/builds/$build/steps")({
  server: {
    handlers: apiRoute<{ build: string }>({
      // A build's id is DEPLOYMENT.SERVICE.
      POST: ({ request, params }) => buildCall(request, (text) => recordStoreGithubBuildSteps(request, params.build, text)),
    }),
  },
});
