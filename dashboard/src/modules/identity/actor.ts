import { Schema } from "effect";

export const Actor = Schema.Struct({
  userId: Schema.String,
});

export type Actor = typeof Actor.Type;

/**
 * Who a request acts as, bound to one Organization. A `ployz` API request is a member, through a signed-in session (a
 * device's active Organization) or an Organization Token; a dashboard request is a member or support, through the
 * session, in the Organization its URL names. It is an Actor, so Actor-taking operations accept it.
 */
export type Caller = Actor & {
  readonly organization: { readonly id: string; readonly slug: string };
  readonly credential: { readonly kind: "session" | "token"; readonly id: string };
};
