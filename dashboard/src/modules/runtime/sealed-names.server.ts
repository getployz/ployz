import "@tanstack/react-start/server-only";
import { Effect, Exit } from "effect";
import { SealedNames } from "@ployz/sdk";
import { sealedNames } from "#/modules/config-store/store-lookup.server";

/** How often an open log read rereads the Sealed names when no reader asks sooner. */
const RECHECK_MS = 5_000;

/**
 * The handle one log request's readers scrub under, fed from the Store for as long as the scope lives. A reader that
 * read a Container's env after the last names read asks for the next one, so a Deploy that seals a variable and starts
 * its Container mid-stream is scrubbed from that Container's first line. A failed read hides every line from then on.
 */
export const sealedFeed = Effect.fn("Runtime.sealedFeed")(function* (organizationId: string, namespace: string, sealed = new SealedNames()) {
  const refresh = Effect.gen(function* () {
    const read = sealed.begin();
    // A defect fails the read too, so the feed keeps running and nothing shows unchecked.
    const names = yield* Effect.exit(sealedNames(organizationId, namespace));
    if (Exit.isSuccess(names)) read.done(names.value);
    else read.failed();
  });
  // The native wait outlives a lost race, so one pending wait carries over until it resolves. A fresh wait each
  // round would leave the old one holding the next ask.
  let pending: Promise<void> | undefined;
  const wanted = Effect.map(Effect.promise(() => (pending ??= sealed.wanted())), () => { pending = undefined; });
  yield* Effect.forkScoped(Effect.forever(Effect.andThen(Effect.race(wanted, Effect.sleep(RECHECK_MS)), refresh)));
  return sealed;
});
