import { Schema } from "effect";

export const Actor = Schema.Struct({
  userId: Schema.String,
});

export type Actor = typeof Actor.Type;

/**
 * Who a `ployz` API request acts as: a member bound to one Organization, through a signed-in session (a device's
 * active Organization) or an Organization Token. It is an Actor, so Actor-taking operations accept it.
 */
export type Caller = Actor & {
  readonly organization: { readonly id: string; readonly slug: string };
  readonly credential: { readonly kind: "session" | "token"; readonly id: string };
};
