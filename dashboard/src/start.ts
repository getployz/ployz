import { createCsrfMiddleware, createIsomorphicFn, createMiddleware, createStart } from "@tanstack/react-start";
import { negotiatePageResponse } from "./server/content-negotiation";
import { installWebShutdown } from "./server/web-shutdown.server";

// Keep shutdown in the same SSR bundle and runtime as application requests.
createIsomorphicFn().server(() => installWebShutdown())();

const negotiationMiddleware = createMiddleware().server(async ({ request, next, handlerType }) => {
  const result = await next();
  if (handlerType !== "router") return result;
  return negotiatePageResponse(request, result.response);
});

export const startInstance = createStart(() => ({
  requestMiddleware: [
    createCsrfMiddleware({ filter: (ctx) => ctx.handlerType === "serverFn" }),
    negotiationMiddleware,
  ],
}));
