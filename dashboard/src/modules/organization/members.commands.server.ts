import "@tanstack/react-start/server-only";
import { Effect, Schema } from "effect";
import { defineCommand, refused } from "#/modules/commands/command.server";
import type { Caller } from "#/modules/identity/actor";
import { createInvite, listPendingInvites, revokeInvite } from "#/modules/organization/invitation.server";
import { listMembers, MANAGE, OWN, removeMembership, setMemberRole } from "#/modules/organization/membership.server";
import { AppConfig } from "#/server/config.server";
import type { Conflict } from "#/server/public-error";

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
});

const Changed = Schema.Struct({ email: Schema.String, role: MemberRole });
const Removed = Schema.Struct({ email: Schema.String });
const Revoked = Schema.Struct({ id: Schema.String, email: Schema.NullOr(Schema.String) });
const Left = Schema.Struct({ organization: Schema.String });

export const manages = (caller: Caller) => caller.role === "owner" || caller.role === "admin";

const conflictRefused = (conflict: Conflict) => Effect.succeed(refused("conflict", conflict.message));

const pick = Effect.fn("Members.pick")(function* (caller: Caller, selector: string) {
  const members = yield* listMembers(caller.organization.id);
  const wanted = selector.trim().toLowerCase();
  const named = members.filter((row) => (wanted.includes("@") ? row.email : row.name.trim()).toLowerCase() === wanted);
  const [only, ...rest] = named;
  if (only === undefined) return refused("not_found", `No member of ${caller.organization.slug} is ${selector}.`);
  if (rest.length > 0) {
    return refused("ambiguous", `${named.length} members are named ${selector}. Pick one by email.`, { candidates: named.map((row) => row.email) });
  }
  return { ok: true, member: only } as const;
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
  about: "Make an invite link that adds whoever opens it as a member. It lasts 7 days and works once",
  examples: ["ployz org member invite", "ployz org member invite --email bob@example.com --role admin"],
  kind: "write",
  gate: "never",
  agents: false,
  typed: null,
  positional: [],
  input: Schema.Struct({
    email: Schema.optionalKey(Schema.NonEmptyString.annotate({ description: "Who the link is for, as a label. Anyone with the link can use it" })),
    role: Schema.optionalKey(InviteRole.annotate({ description: "The role it grants. member when left out" })),
  }),
  output: Invited,
  render: "record",
  run: (caller, input) => Effect.gen(function* () {
    if (!manages(caller)) return refused("forbidden", MANAGE);
    const config = yield* AppConfig;
    const invite = yield* createInvite(caller.organization.id, caller.userId, { email: input.email ?? null, role: input.role ?? "member" });
    return {
      ok: true,
      output: {
        id: invite.id,
        email: invite.email,
        role: invite.role,
        link: new URL(`/invite/${invite.token}`, config.app.url).toString(),
        expires_at: invite.expiresAt.toISOString(),
      },
      message: "Send this link to the person you are inviting. It is shown only once.",
    } as const;
  }).pipe(Effect.catchTag("Conflict", conflictRefused)),
});

export const orgMemberRole = defineCommand({
  name: "org_member_role",
  path: ["org", "member", "role"],
  about: "Change a member's role. Only an owner can make or remove an owner",
  examples: ["ployz org member role bob@example.com admin"],
  kind: "write",
  gate: "never",
  agents: false,
  typed: null,
  positional: ["member", "role"],
  input: Schema.Struct({ member: MemberSelector, role: MemberRole.annotate({ description: "owner, admin or member" }) }),
  output: Changed,
  render: "message",
  run: (caller, input) => Effect.gen(function* () {
    if (!manages(caller)) return refused("forbidden", MANAGE);
    const picked = yield* pick(caller, input.member);
    if (!picked.ok) return picked;
    const target = picked.member;
    if ((input.role === "owner" || target.role === "owner") && caller.role !== "owner") return refused("forbidden", OWN);
    yield* setMemberRole(caller.organization.id, target.userId, input.role, caller.userId);
    return { ok: true, output: { email: target.email, role: input.role }, message: `${target.email} is now ${input.role}.` } as const;
  }).pipe(Effect.catchTag("Conflict", conflictRefused)),
});

export const orgMemberRm = defineCommand({
  name: "org_member_rm",
  path: ["org", "member", "rm"],
  about: "Remove a member. Their Organization Tokens stop working and their devices lose access to its Servers",
  examples: ["ployz org member rm bob@example.com"],
  kind: "write",
  gate: "never",
  agents: false,
  typed: null,
  positional: ["member"],
  input: Schema.Struct({ member: MemberSelector }),
  output: Removed,
  render: "message",
  run: (caller, input) => Effect.gen(function* () {
    if (!manages(caller)) return refused("forbidden", MANAGE);
    const picked = yield* pick(caller, input.member);
    if (!picked.ok) return picked;
    const target = picked.member;
    if (target.role === "owner" && caller.role !== "owner") return refused("forbidden", OWN);
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
  gate: "never",
  agents: false,
  typed: null,
  positional: ["invite"],
  input: Schema.Struct({ invite: Schema.NonEmptyString.annotate({ description: "The invite, by id or by the email it names" }) }),
  output: Revoked,
  render: "message",
  run: (caller, input) => Effect.gen(function* () {
    if (!manages(caller)) return refused("forbidden", MANAGE);
    const revoked = yield* revokeInvite(caller.organization.id, input.invite);
    return { ok: true, output: revoked, message: `Revoked the invite${revoked.email === null ? "" : ` for ${revoked.email}`}. Its link no longer works.` } as const;
  }).pipe(Effect.catchTag("Conflict", conflictRefused)),
});

export const orgLeave = defineCommand({
  name: "org_leave",
  path: ["org", "leave"],
  about: "Leave the Organization. Its last owner cannot leave",
  examples: ["ployz org leave"],
  kind: "write",
  gate: "never",
  agents: false,
  typed: null,
  positional: [],
  input: Schema.Struct({}),
  output: Left,
  render: "message",
  run: (caller) => removeMembership(caller.organization.id, caller.userId, caller.userId).pipe(
    Effect.as({ ok: true, output: { organization: caller.organization.slug }, message: `You left ${caller.organization.slug}.` } as const),
    Effect.catchTag("Conflict", conflictRefused),
  ),
});
