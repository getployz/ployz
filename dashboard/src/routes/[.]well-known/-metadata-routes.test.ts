import { expect, it } from "vitest";
import { routeTree } from "#/routeTree.gen";
import { createRouter } from "@tanstack/react-router";

// Matching reads no context, so the loaders' services are never needed.
const router = createRouter({ routeTree, context: {} as never });

it.each(["/.well-known/oauth-protected-resource", "/.well-known/oauth-protected-resource/mcp"])(
  "%s has a server route, not the app's 404 page",
  (path) => {
    const [, , found] = router.getMatchedRoutes(path);
    expect(found?.options.server?.handlers).toHaveProperty("ANY");
  },
);
