import "@tanstack/react-start/server-only";
import { getRequest } from "@tanstack/react-start/server";
import { Context, Deferred, Effect, Exit } from "effect";

/**
 * What one incoming HTTP request has looked up so far. During SSR every server function a page's loaders call runs
 * in-process under the page's own Request, so a key here is read once per page. Nothing outlives its Request, so a
 * revoked session or a removed member is refused on the next one.
 */
export type RequestScopeService = {
  /** `effect`'s result for `key` in this request: the first caller runs it, later ones share it. A failure is forgotten. */
  readonly once: <A, E, R>(key: string, effect: Effect.Effect<A, E, R>) => Effect.Effect<A, E, R>;
};

/** The request being served. Outside one (Inngest, API routes, tests) nothing is remembered. */
export const RequestScope = Context.Reference<RequestScopeService>("ployz/RequestScope", {
  defaultValue: () => ({ once: (_key, effect) => effect }),
});

const remembering = (): RequestScopeService => {
  const entries = new Map<string, unknown>();
  return {
    once: <A, E, R>(key: string, effect: Effect.Effect<A, E, R>) =>
      Effect.suspend(() => {
        // SAFETY: a key is written only by the `once` call whose effect it names, so its Deferred has that effect's types.
        const pending = entries.get(key) as Deferred.Deferred<A, E> | undefined;
        if (pending !== undefined) return Deferred.await(pending);
        const deferred = Deferred.makeUnsafe<A, E>();
        entries.set(key, deferred);
        return Effect.onExit(effect, (exit) => {
          if (!Exit.isSuccess(exit)) entries.delete(key);
          return Deferred.done(deferred, exit);
        });
      }),
  };
};

const scopes = new WeakMap<Request, RequestScopeService>();

/** The scope of the request being served. Call it before an Effect runs: inside a fiber `getRequest()` may name another. */
export function currentRequestScope() {
  const request = getRequest();
  let scope = scopes.get(request);
  if (scope === undefined) {
    scope = remembering();
    scopes.set(request, scope);
  }
  return scope;
}
