import { createCsrfMiddleware, createIsomorphicFn, createMiddleware, createStart } from "@tanstack/react-start";
import { secretPagesOf } from "./lib/secret-pages";
import { serverFnFetch } from "./lib/server-fn-fetch";
import { getRouter } from "./router";
import { negotiatePageResponse } from "./server/content-negotiation";
import { unhandledRequestMiddleware } from "./server/unhandled-request";
import { installWebShutdown } from "./server/web-shutdown.server";

// Keep shutdown in the same SSR bundle and runtime as application requests.
createIsomorphicFn().server(() => installWebShutdown())();

let routerSecretPages: ((pathname: string) => boolean) | undefined;
const opensSecretPage = (pathname: string): boolean => (routerSecretPages ??= secretPagesOf(getRouter()))(pathname);

const negotiationMiddleware = createMiddleware().server(async ({ request, next, handlerType }) => {
  const result = await next();
  if (handlerType !== "router") return result;
  return negotiatePageResponse(request, result.response);
});

export const startInstance = createStart(() => ({
  requestMiddleware: [
    unhandledRequestMiddleware(opensSecretPage),
    createCsrfMiddleware({ filter: (ctx) => ctx.handlerType === "serverFn" }),
    negotiationMiddleware,
  ],
  serverFns: { fetch: serverFnFetch },
}));
