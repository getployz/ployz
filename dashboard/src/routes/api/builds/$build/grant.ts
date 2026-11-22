import { createFileRoute } from "@tanstack/react-router";
import { grantRuleBuild } from "#/modules/config-store/store-github-builds.server";
import { buildCall } from "#/routes/api/builds/-build-call";
import { apiRoute } from "#/server/api-route";

/** A rule build's grant ask, relayed to a Machine that checks it. Authenticated by the runner's OIDC token. */
export const Route = createFileRoute("/api/builds/$build/grant")({
  server: {
    handlers: apiRoute<{ build: string }>({ POST: ({ request, params }) => buildCall(request, (text) => grantRuleBuild(request, params.build, text)) }),
  },
});
