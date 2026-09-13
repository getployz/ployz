import "@tanstack/react-start/server-only";
import { serve } from "inngest/edge";
import { Data, Effect } from "effect";
import type { PloyzInngest } from "#/modules/inngest/client";
import { createInngestFunctions } from "#/modules/inngest/index";

export class InngestRequestError extends Data.TaggedError("InngestRequestError")<{
  readonly cause: unknown;
}> {}

type InngestServeHandler = (request: Request) => Promise<Response>;

type CachedInngestServe = {
  readonly functions: ReturnType<typeof createInngestFunctions>;
  readonly handlersByOrigin: Map<string, InngestServeHandler>;
};

const serveCache = new WeakMap<PloyzInngest, CachedInngestServe>();

function inngestServeHandler(inngest: PloyzInngest, serveOrigin: string) {
  let cached = serveCache.get(inngest);
  if (cached === undefined) {
    cached = {
      functions: createInngestFunctions(inngest),
      handlersByOrigin: new Map(),
    };
    serveCache.set(inngest, cached);
  }
  const existing = cached.handlersByOrigin.get(serveOrigin);
  if (existing !== undefined) return existing;
  const handler = serve({
    client: inngest,
    functions: cached.functions,
    serveOrigin,
    servePath: "/api/inngest",
  });
  cached.handlersByOrigin.set(serveOrigin, handler);
  return handler;
}

export function handleInngestHttp(
  inngest: PloyzInngest,
  serveOrigin: string,
  request: Request,
) {
  const handler = inngestServeHandler(inngest, serveOrigin);
  return Effect.tryPromise({
    try: () => handler(request),
    catch: (cause) => new InngestRequestError({ cause }),
  });
}
