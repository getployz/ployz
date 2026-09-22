import { createCsrfMiddleware, createIsomorphicFn, createStart } from "@tanstack/react-start";
import { installWebShutdown } from "./server/web-shutdown.server";

// Keep shutdown in the same SSR bundle and runtime as application requests.
createIsomorphicFn().server(() => installWebShutdown())();

export const startInstance = createStart(() => ({
  requestMiddleware: [createCsrfMiddleware({ filter: (ctx) => ctx.handlerType === "serverFn" })],
}));
