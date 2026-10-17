import { Schema } from "effect";
import { type Permission, permissionsOf } from "#/modules/organization/permissions";
import type { SupportShows } from "#/modules/support-access/support-access";
import { granted, type TokenGrant } from "#/modules/identity/token-grant";

export const Actor = Schema.Struct({
  userId: Schema.String,
});

/** A user, and the credential they act through: the Organization gate narrows a token's Access to its grant. */
export type Actor = typeof Actor.Type & { readonly credential?: Credential };

/** A session (the dashboard or `ployz login`), an Organization Token, or a remote agent's OAuth grant. */
export type CredentialKind = "session" | "token" | "oauth";

export const TOKEN_KINDS = ["cli", "agent"] as const;
export type TokenKind = (typeof TOKEN_KINDS)[number];

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

type Holder = Pick<Caller, "role" | "support"> & Partial<Pick<Caller, "credential">>;

const heldBy = (caller: Holder): Permission[] => {
  const held = caller.role !== null ? permissionsOf(caller.role) : caller.support !== null ? permissionsOf("support") : [];
  return caller.credential?.kind === "token" ? granted(held, caller.credential.grant) : held;
};

export const callerCan = (caller: Omit<Holder, "support">, permission: Permission) =>
  caller.role !== null && heldBy({ ...caller, support: null }).includes(permission);

/** `callerCan`, but support with support access holds what the support role does: for the sites that let support act. */
export const callerOrSupportCan = (caller: Holder, permission: Permission) =>
  heldBy(caller).includes(permission);

export const callerPermissions = (caller: Holder): Permission[] => heldBy(caller);

export const callerEnvironments = (caller: Pick<Caller, "credential">): string[] | undefined =>
  caller.credential.kind === "token" && caller.credential.grant.environments !== null ? [...caller.credential.grant.environments] : undefined;
