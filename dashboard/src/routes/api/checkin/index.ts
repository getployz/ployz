import { createFileRoute } from "@tanstack/react-router";
import { Effect } from "effect";
import { handleCheckin } from "#/routes/api/checkin/-handler";
import { apiRoute } from "#/server/api-route";

export const Route = createFileRoute("/api/checkin/")({
  server: {
    handlers: apiRoute({ POST: () => Effect.sync(handleCheckin) }),
  },
});
