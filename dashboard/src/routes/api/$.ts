import { createFileRoute } from "@tanstack/react-router";
import { NotFound, publicErrorResponse } from "#/server/public-error";

// Without this, an unknown /api path falls to the root splat and answers an API client with an HTML page.
export const Route = createFileRoute("/api/$")({
  server: {
    handlers: {
      ANY: () => publicErrorResponse(new NotFound({ message: "No API route at this path." })),
    },
  },
});
