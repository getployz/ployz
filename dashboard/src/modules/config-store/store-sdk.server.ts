import "@tanstack/react-start/server-only";
import { Effect } from "effect";

/**
 * A call into the Config Store's native SDK, as an Effect failing with what it rejected with: a Store refusal
 * (`RpcError`) or a fault. The one place the Store's promises become Effects.
 */
export const storeTry = <A>(call: () => Promise<A>) => Effect.tryPromise({ try: call, catch: (cause) => cause });
