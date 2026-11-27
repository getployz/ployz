import "@tanstack/react-start/server-only";
import { Clock, Context, Deferred, Effect, Exit, Layer, Schedule, Scope } from "effect";
import { accessFor } from "./github-access";
import { GithubApi, type GithubReadError } from "./github-observation.api";
import {
  authorizeRepository, extractGithubArchive, type GithubSourceError, makeSourceWorkspace, removeSourceWorkspace, type SourceIdentity,
} from "./github-source.server";

const IDLE_MS = 10 * 60_000;
/** Checkouts no Deployment holds, newest kept, past which the oldest goes at once. */
const IDLE_KEPT = 2;

type Pin = SourceIdentity & { sha: string };
type Authorized = ReturnType<typeof authorizeRepository>;
type Failure = GithubSourceError | GithubReadError;
type Checkout = { readonly ready: Deferred.Deferred<string, Failure>; workspace: string | null; holders: number; idleSince: number | null };

export interface GithubSourcesService {
  /** Start checking out a pinned commit, if the Organization may still read it, for a later `checkout` to find. */
  readonly prefetch: (pin: Pin) => Effect.Effect<void, never, GithubApi | Effect.Services<Authorized>>;
  /**
   * The repository's directory at the pinned commit, until the scope closes. Every Service at one commit shares it, and
   * reads it only. The Organization's access is checked on each call; a failed checkout is forgotten.
   */
  readonly checkout: (pin: Pin) => Effect.Effect<string, Failure | Effect.Error<Authorized>, Scope.Scope | GithubApi | Effect.Services<Authorized>>;
}

export class GithubSources extends Context.Service<GithubSources, GithubSourcesService>()("ployz/GithubSources") {}

const keyOf = (pin: Pin) => `${pin.organizationId}:${pin.repositoryId}@${pin.sha}`;

export const makeGithubSources = Effect.gen(function* () {
  const scope = yield* Effect.scope;
  const checkouts = new Map<string, Checkout>();
  const remove = (workspace: string | null) => workspace === null ? Effect.void : removeSourceWorkspace(workspace);
  const forget = (key: string, checkout: Checkout) => Effect.suspend(() => {
    if (checkouts.get(key) === checkout) checkouts.delete(key);
    return remove(checkout.workspace);
  });
  const evict = Effect.gen(function* () {
    const now = yield* Clock.currentTimeMillis;
    const idle = [...checkouts].filter(([, checkout]) => checkout.idleSince !== null)
      .sort(([, a], [, b]) => (b.idleSince ?? 0) - (a.idleSince ?? 0));
    const gone = idle.filter(([, checkout], index) => index >= IDLE_KEPT || now - (checkout.idleSince ?? now) >= IDLE_MS);
    for (const [key] of gone) checkouts.delete(key);
    yield* Effect.forEach(gone, ([, checkout]) => remove(checkout.workspace), { discard: true });
  });
  yield* Scope.addFinalizer(scope, Effect.suspend(() => Effect.forEach([...checkouts], ([key, checkout]) => forget(key, checkout), { discard: true })));
  yield* evict.pipe(Effect.repeat(Schedule.spaced("1 minute")), Effect.forkIn(scope));

  const fetch = (pin: Pin, repository: Effect.Success<Authorized>, checkout: Checkout) => Effect.gen(function* () {
    const workspace = yield* makeSourceWorkspace;
    checkout.workspace = workspace;
    const api = yield* GithubApi;
    const response = yield* api.archive({ access: accessFor(pin.organizationId, pin.installationId), repository, sha: pin.sha });
    return yield* extractGithubArchive(response, workspace);
  });

  const ensure = Effect.fn("GithubSources.ensure")(function* (pin: Pin, hold: boolean) {
    const repository = yield* authorizeRepository(pin);
    const key = keyOf(pin);
    const ready = yield* Deferred.make<string, Failure>();
    const found = checkouts.get(key);
    const checkout: Checkout = found ?? { ready, workspace: null, holders: 0, idleSince: null };
    if (hold) { checkout.holders += 1; checkout.idleSince = null; }
    if (found !== undefined) return checkout;
    checkouts.set(key, checkout);
    const services = yield* Effect.context<GithubApi>();
    yield* fetch(pin, repository, checkout).pipe(
      Effect.provideContext(services),
      Effect.onExit((exit) => Exit.isSuccess(exit) ? Effect.void : forget(key, checkout)),
      Effect.exit,
      Effect.flatMap((exit) => Deferred.done(checkout.ready, exit)),
      Effect.andThen(Clock.currentTimeMillis),
      Effect.flatMap((now) => Effect.suspend(() => {
        if (checkout.holders === 0) checkout.idleSince = now;
        return evict;
      })),
      Effect.forkIn(scope),
    );
    yield* evict;
    return checkout;
  });

  const prefetch: GithubSourcesService["prefetch"] = (pin) => ensure(pin, false).pipe(
    Effect.catchCause((cause) => Effect.logWarning("Could not start a source checkout early.", cause)),
    Effect.forkIn(scope),
    Effect.asVoid,
  );

  const checkout: GithubSourcesService["checkout"] = Effect.fn("GithubSources.checkout")(function* (pin: Pin) {
    const held = yield* Effect.acquireRelease(
      ensure(pin, true),
      (checkout) => Clock.currentTimeMillis.pipe(
        Effect.tap((now) => Effect.sync(() => { checkout.holders -= 1; if (checkout.holders === 0) checkout.idleSince = now; })),
        Effect.andThen(evict),
      ),
    );
    return yield* Deferred.await(held.ready);
  });

  return { prefetch, checkout } satisfies GithubSourcesService;
});

/** Pinned checkouts, one per repository and commit, shared by a Deployment's Services and kept a while for the next. */
export const GithubSourcesLive = Layer.effect(GithubSources, makeGithubSources);
