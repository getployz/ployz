import { Schema } from "effect";
import type { SupportShows } from "#/modules/support-access/support-access";

export const Actor = Schema.Struct({
  userId: Schema.String,
});

export type Actor = typeof Actor.Type;

/**
 * Who a request acts as, bound to one Organization. A `ployz` API request is a member, through a signed-in session (a
 * device's active Organization) or an Organization Token; a dashboard request is a member or support, through the
 * session, in the Organization its URL names. It is an Actor, so Actor-taking operations accept it. `role` is a member's
 * role there, null for support; `support` is what Ployz support may read there, null for a member. `via` is who started
 * the request, set by Cloud from the route it came in on: a person in the dashboard, a `ployz` command, or an agent (the
 * sidebar or `ployz mcp`).
 */
export type Caller = Actor & {
  readonly organization: { readonly id: string; readonly slug: string };
  readonly credential: { readonly kind: "session" | "token"; readonly id: string };
  readonly role: "owner" | "admin" | "member" | null;
  readonly support: SupportShows | null;
  readonly via: "dashboard" | "cli" | "agent";
};
