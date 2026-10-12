import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { manifest } from "#/modules/commands/commands.server";
import { serveCli } from "#/routes/api/cli/-handlers";

/** The commands Cloud publishes, for `ployz` to cache and show beside its own. Names nothing about any Organization. */
export const Route = createFileRoute("/api/cli/manifest")({
  server: {
    handlers: {
      GET: async ({ request }) => serveCli(request, Effect.sync(() => Response.json(manifest(), { headers: { "cache-control": "no-store" } }))),
    },
  },
});
