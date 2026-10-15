import "@tanstack/react-start/server-only";
import type { JsonValue } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { defineCommand, refused } from "#/modules/commands/command.server";
import { StoreRefused } from "#/modules/config-store/store.contract";
import type { Caller } from "#/modules/identity/actor";
import { callerOrganizations } from "#/modules/identity/caller.server";
import type { Role } from "#/modules/organization/access.server";
import { emailInvite } from "#/modules/organization/invitation-mail.server";
import { createInvite, INVITE_DAYS, listPendingInvites, revokeInvite } from "#/modules/organization/invitation.server";
import { listMembers, MANAGE, OWN, removeMembership, setMemberRole } from "#/modules/organization/membership.server";
import { AppConfig } from "#/server/config.server";
import { type Conflict, NotFound } from "#/server/public-error";

const MemberSelector = Schema.NonEmptyString.annotate({ description: "The member, by email or name" });
const MemberRole = Schema.Literals(["owner", "admin", "member"]);
const InviteRole = Schema.Literals(["admin", "member"]);

const Member = Schema.Struct({
  name: Schema.String,
  email: Schema.String,
  role: MemberRole,
  joined_at: Schema.String,
  you: Schema.Boolean,
});

const Invite = Schema.Struct({
  id: Schema.String,
  email: Schema.NullOr(Schema.String),
  role: InviteRole,
  invited_by: Schema.String,
  expires_at: Schema.String,
});

const Invited = Schema.Struct({
  id: Schema.String,
  email: Schema.NullOr(Schema.String),
  role: InviteRole,
  link: Schema.String,
  expires_at: Schema.String,
  emailed_to: Schema.NullOr(Schema.String),
});

/** One address, so a mailed invite has exactly one recipient. */
const InviteEmail = Schema.String.check(Schema.isPattern(/^[^\s@]+@[^\s@]+\.[^\s@]+$/u, {
  message: "Enter one email address, like bob@example.com",
}));

const Changed = Schema.Struct({ email: Schema.String, role: MemberRole });
const Removed = Schema.Struct({ email: Schema.String });
const Revoked = Schema.Struct({ id: Schema.String, email: Schema.NullOr(Schema.String) });
const Left = Schema.Struct({ organization: Schema.String });

export const manages = (caller: Caller) => caller.role === "owner" || caller.role === "admin";

const conflictRefused = (conflict: Conflict) => Effect.succeed(refused("conflict", conflict.message));

const refuse = (code: string, message: string, details: JsonValue = null) => new StoreRefused({ code, message, details });

/**
 * The member `selector` names, by email when it holds an `@` and by name otherwise. Refuses `forbidden` unless the
 * caller manages members, and unless they are an owner when `touchesOwner` says the change makes or removes one.
 */
const managed = Effect.fn("Members.managed")(function* (caller: Caller, selector: string, touchesOwner: (role: Role) => boolean) {
  if (!manages(caller)) return yield* refuse("forbidden", MANAGE);
  const members = yield* listMembers(caller.organization.id);
  const wanted = selector.trim().toLowerCase();
  const named = members.filter((row) => (wanted.includes("@") ? row.email : row.name.trim()).toLowerCase() === wanted);
  const [only, ...rest] = named;
  if (only === undefined) return yield* refuse("not_found", `No member of ${caller.organization.slug} is ${selector}.`);
  if (rest.length > 0) {
    return yield* refuse("ambiguous", `${named.length} members are named ${selector}: ${named.map((row) => row.email).join(", ")}. Pick one by email.`, { candidates: named.map((row) => row.email) });
  }
  if (touchesOwner(only.role) && caller.role !== "owner") return yield* refuse("forbidden", OWN);
  return only;
});

const pendingInvite = Effect.fn("Members.pendingInvite")(function* (caller: Caller, selector: string) {
  if (!manages(caller)) return yield* refuse("forbidden", MANAGE);
  const wanted = selector.trim();
  const invites = yield* listPendingInvites(caller.organization.id);
  const found = invites.find((row) => row.id === wanted) ?? invites.find((row) => row.email === wanted.toLowerCase());
  if (found === undefined) return yield* new NotFound({ message: `No pending invite matches ${selector}.` });
  return found;
});

export const orgMemberLs = defineCommand({
  name: "org_member_ls",
  path: ["org", "member", "ls"],
  about: "List the Organization's members and their roles",
  examples: ["ployz org member ls"],
  kind: "read",
  agents: true,
  typed: null,
  positional: [],
  input: Schema.Struct({}),
  output: Schema.Array(Member),
  render: "table",
  run: (caller) => listMembers(caller.organization.id).pipe(Effect.map((members) => ({
    ok: true,
    output: members.map((row) => ({
      name: row.name,
      email: row.email,
      role: row.role,
      joined_at: row.joinedAt.toISOString(),
      you: row.userId === caller.userId,
    })),
  }) as const)),
});

export const orgMemberInvite = defineCommand({
  name: "org_member_invite",
  path: ["org", "member", "invite"],
  about: `Make an invite link that adds whoever opens it as a member. It lasts ${INVITE_DAYS} days and works once`,
  examples: ["ployz org member invite bob@example.com", "ployz org member invite bob@example.com --role admin"],
  kind: "write",
  removes: false,
  gate: "never",
  agents: false,
  typed: null,
  positional: ["email"],
  input: Schema.Struct({
    email: Schema.optionalKey(InviteEmail.annotate({ description: "Who the link is for. Cloud emails it there when email is on. Anyone with the link can use it" })),
    role: Schema.optionalKey(InviteRole.annotate({ description: "The role it grants: admin or member. member when left out" })),
  }),
  output: Invited,
  render: "message",
  run: (caller, input) => Effect.gen(function* () {
    if (!manages(caller)) return refused("forbidden", MANAGE);
    const config = yield* AppConfig;
    const invite = yield* createInvite(caller.organization.id, caller.userId, { email: input.email ?? null, role: input.role ?? "member" });
    const link = new URL(`/invite/${invite.token}`, config.app.url).toString();
    const emailedTo = yield* emailInvite(invite, caller.organization.id, caller.userId, link);
    return {
      ok: true,
      output: { id: invite.id, email: invite.email, role: invite.role, link, expires_at: invite.expiresAt.toISOString(), emailed_to: emailedTo },
      message: emailedTo === null
        ? `Send this link to the person you're inviting. It works once and lasts ${INVITE_DAYS} days.\n${link}`
        : `Emailed to ${emailedTo}. The link works once and lasts ${INVITE_DAYS} days.\n${link}`,
    } as const;
  }).pipe(Effect.catchTag("Conflict", conflictRefused)),
});

const RoleInput = Schema.Struct({ member: MemberSelector, role: MemberRole.annotate({ description: "The new role: owner, admin or member" }) });

const roleTarget = (caller: Caller, input: typeof RoleInput.Type) =>
  managed(caller, input.member, (role) => role === "owner" || input.role === "owner");

export const orgMemberRole = defineCommand({
  name: "org_member_role",
  path: ["org", "member", "role"],
  about: "Change a member's role to owner, admin or member. Only an owner can make or remove an owner",
  examples: ["ployz org member role bob@example.com admin", "ployz org member role Bob member"],
  kind: "write",
  removes: false,
  gate: {
    gate: "ask",
    asked: (caller, input) => roleTarget(caller, input).pipe(Effect.map((target) => ({
      subject: `member:${target.userId}`,
      verb: "set",
      name: target.email,
      preview: { email: target.email, from: target.role, to: input.role },
      effects: target.role === input.role ? [] : [{ kind: "changes_role", name: target.email, node: target.userId, path: `members/${target.userId}` }],
    }))),
  },
  agents: true,
  typed: null,
  positional: ["member", "role"],
  input: RoleInput,
  output: Changed,
  render: "message",
  run: (caller, input) => Effect.gen(function* () {
    const target = yield* roleTarget(caller, input);
    yield* setMemberRole(caller.organization.id, target.userId, input.role, caller.userId);
    return { ok: true, output: { email: target.email, role: input.role }, message: `${target.email} is now ${input.role}.` } as const;
  }).pipe(Effect.catchTag("Conflict", conflictRefused)),
});

const rmTarget = (caller: Caller, member: string) => managed(caller, member, (role) => role === "owner");

export const orgMemberRm = defineCommand({
  name: "org_member_rm",
  path: ["org", "member", "rm"],
  about: "Remove a member. Their Organization Tokens stop working and their devices lose access to its Servers",
  examples: ["ployz org member rm bob@example.com", "ployz org member rm Bob"],
  kind: "write",
  removes: true,
  gate: {
    gate: "ask",
    asked: (caller, input) => rmTarget(caller, input.member).pipe(Effect.map((target) => ({
      subject: `member:${target.userId}`,
      verb: "remove",
      name: target.email,
      preview: { email: target.email, role: target.role },
      effects: [{ kind: "removes_member", name: target.email, node: target.userId, path: `members/${target.userId}` }],
    }))),
  },
  agents: true,
  typed: null,
  positional: ["member"],
  input: Schema.Struct({ member: MemberSelector }),
  output: Removed,
  render: "message",
  run: (caller, input) => Effect.gen(function* () {
    const target = yield* rmTarget(caller, input.member);
    yield* removeMembership(caller.organization.id, target.userId, caller.userId);
    return { ok: true, output: { email: target.email }, message: `${target.email} is no longer a member of ${caller.organization.slug}.` } as const;
  }).pipe(Effect.catchTag("Conflict", conflictRefused)),
});

export const orgInviteLs = defineCommand({
  name: "org_invite_ls",
  path: ["org", "invite", "ls"],
  about: "List the Organization's pending invites",
  examples: ["ployz org invite ls"],
  kind: "read",
  agents: true,
  typed: null,
  positional: [],
  input: Schema.Struct({}),
  output: Schema.Array(Invite),
  render: "table",
  run: (caller) => !manages(caller) ? Effect.succeed(refused("forbidden", MANAGE)) : listPendingInvites(caller.organization.id).pipe(Effect.map((invites) => ({
    ok: true,
    output: invites.map((row) => ({
      id: row.id,
      email: row.email,
      role: row.role,
      invited_by: row.invitedBy,
      expires_at: row.expiresAt.toISOString(),
    })),
  }) as const)),
});

export const orgInviteRevoke = defineCommand({
  name: "org_invite_revoke",
  path: ["org", "invite", "revoke"],
  about: "Revoke a pending invite so its link stops working",
  examples: ["ployz org invite revoke bob@example.com"],
  kind: "write",
  removes: true,
  gate: {
    gate: "ask",
    asked: (caller, input) => pendingInvite(caller, input.invite).pipe(Effect.map((invite) => ({
      subject: `invite:${invite.id}`,
      verb: "remove",
      name: invite.email ?? invite.id,
      preview: { id: invite.id, email: invite.email, role: invite.role },
      effects: [{ kind: "revokes_invite", name: invite.email ?? invite.id, node: invite.id, path: `invites/${invite.id}` }],
    }))),
  },
  agents: true,
  typed: null,
  positional: ["invite"],
  input: Schema.Struct({ invite: Schema.NonEmptyString.annotate({ description: "The invite, by id or by the email it names" }) }),
  output: Revoked,
  render: "message",
  run: (caller, input) => Effect.gen(function* () {
    const invite = yield* pendingInvite(caller, input.invite);
    const revoked = yield* revokeInvite(caller.organization.id, invite.id);
    return { ok: true, output: revoked, message: `Revoked the invite${revoked.email === null ? "" : ` for ${revoked.email}`}. Its link no longer works.` } as const;
  }).pipe(Effect.catchTag("Conflict", conflictRefused)),
});

export const orgLeave = defineCommand({
  name: "org_leave",
  path: ["org", "leave"],
  about: "Leave the Organization. Its last owner cannot leave",
  examples: ["ployz org leave"],
  kind: "write",
  removes: true,
  gate: "never",
  agents: false,
  typed: null,
  positional: [],
  input: Schema.Struct({ organization: Schema.NonEmptyString.annotate({ description: "The Organization to leave, by slug" }) }),
  output: Left,
  render: "message",
  // Leaves the Organization the input names, never the session's active one: a browser moves every session of the
  // user, so the active one can differ from the Organization `ployz` confirmed.
  run: (caller, input) => Effect.gen(function* () {
    const leaving = (yield* callerOrganizations(caller)).find((row) => row.slug === input.organization);
    if (leaving === undefined) return refused("not_found", `You are not a member of Organization ${input.organization}. Nothing changed.`);
    yield* removeMembership(leaving.id, caller.userId, caller.userId);
    return { ok: true, output: { organization: leaving.slug }, message: `You left ${leaving.slug}.` } as const;
  }).pipe(Effect.catchTag("Conflict", conflictRefused)),
});
