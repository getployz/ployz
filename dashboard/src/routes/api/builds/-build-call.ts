import { Effect } from "effect";

/** A runner's call: the body is read first as text, which the Store checks against the runner's claims. */
export const buildCall = <E, R>(request: Request, call: (text: string) => Effect.Effect<unknown, E, R>) =>
  Effect.flatMap(Effect.promise(() => request.text()), (text) => Effect.map(call(text), (body) => Response.json(body)));
