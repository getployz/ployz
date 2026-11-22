import { createFileRoute } from "@tanstack/react-router";
import { checkInRuleBuild } from "#/modules/config-store/store-github-builds.server";
import { buildCall } from "#/routes/api/builds/-build-call";
import { apiRoute } from "#/server/api-route";

/** A rule build's check-in, on the run its push started, authenticated by its OIDC token. */
export const Route = createFileRoute("/api/builds/check-in")({
  server: {
    handlers: apiRoute({ POST: ({ request }) => buildCall(request, (text) => checkInRuleBuild(request, text)) }),
  },
});
