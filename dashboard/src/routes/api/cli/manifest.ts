import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { manifest } from "#/modules/commands/commands.server";
import { cliRoute } from "#/server/api-route";

/** The commands Cloud publishes, for `ployz` to cache and show beside its own. Names nothing about any Organization. */
export const Route = createFileRoute("/api/cli/manifest")({
  server: {
    handlers: cliRoute({
      GET: () => Effect.sync(() => Response.json(manifest(), { headers: { "cache-control": "no-store" } })),
    }),
  },
});
