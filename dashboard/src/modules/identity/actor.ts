import { Schema } from "effect";
import { type Permission, permissionsOf } from "#/modules/organization/permissions";
import type { SupportShows } from "#/modules/support-access/support-access";
import { granted, type TokenGrant } from "#/modules/identity/token-grant";

export const Actor = Schema.Struct({
  userId: Schema.String,
});

export type Actor = typeof Actor.Type;

/** A session (the dashboard or `ployz login`), an Organization Token, or a remote agent's OAuth grant. */
export type CredentialKind = "session" | "token" | "oauth";

export const TOKEN_KINDS = ["cli", "agent"] as const;
export type TokenKind = (typeof TOKEN_KINDS)[number];

/** How a request signed in. Only a token carries a grant, which narrows its maker's role. */
export type Credential =
  | { readonly kind: "session" | "oauth"; readonly id: string }
  | { readonly kind: "token"; readonly id: string; readonly grant: TokenGrant };

/**
 * Who a request acts as, bound to one Organization. A `ployz` API request is a member, through a signed-in session (a
 * device's active Organization) or an Organization Token; a dashboard request is a member or support, through the
 * session, in the Organization its URL names; a remote agent is a member through an OAuth grant bound to one
 * Organization. It is an Actor, so Actor-taking operations accept it. `role` is a member's
 * role there, null for support; `support` is what Ployz support may read there, null for a member.
 */
export type Caller = Actor & {
  readonly organization: { readonly id: string; readonly slug: string };
  readonly credential: Credential;
  readonly role: "owner" | "admin" | "member" | null;
  readonly support: SupportShows | null;
  readonly via: "dashboard" | "cli" | "agent";
};

/** A caller without a credential, such as a dashboard Actor resolved to a role, holds no grant. */
type Holder = Pick<Caller, "role" | "support"> & Partial<Pick<Caller, "credential">>;

const heldBy = (caller: Holder): Permission[] => {
  const held = caller.role !== null ? permissionsOf(caller.role) : caller.support !== null ? permissionsOf("support") : [];
  return caller.credential?.kind === "token" ? granted(held, caller.credential.grant) : held;
};

/** Whether the Caller's member role, narrowed by a token's grant, holds `permission`. Support has no member role, so it holds none here. */
export const callerCan = (caller: Omit<Holder, "support">, permission: Permission) =>
  caller.role !== null && heldBy({ ...caller, support: null }).includes(permission);

/** `callerCan`, but support with support access holds what the support role does: for the sites that let support act. */
export const callerOrSupportCan = (caller: Holder, permission: Permission) =>
  heldBy(caller).includes(permission);

/** The Caller's effective permissions, the set the Store checks each command against: their role's, or support's, narrowed by a token's grant. */
export const callerPermissions = (caller: Holder): Permission[] => heldBy(caller);

/** The Environments a token may write, the Store's ceiling; undefined when it may write any. */
export const callerEnvironments = (caller: Pick<Caller, "credential">): string[] | undefined =>
  caller.credential.kind === "token" && caller.credential.grant.environments !== null ? [...caller.credential.grant.environments] : undefined;
