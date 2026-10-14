import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import type { JsonValue } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { type CommandName, runCommand } from "#/modules/commands/commands.server";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { acceptInvite, readInvite } from "#/modules/organization/invitation.server";
import { Validation } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Slug = Schema.NonEmptyString;
const MemberRole = Schema.Literals(["owner", "admin", "member"]);

/** Runs a member command as the signed-in user in `organizationSlug`; a refusal reaches the form as its message. */
const ran = <N extends CommandName>(organizationSlug: string, name: N, input: JsonValue) => Effect.gen(function* () {
  const caller = yield* dashboardCaller(getRequest().headers, organizationSlug);
  const result = yield* runCommand(caller, name, input, { approvalId: null, key: null });
  if (!result.ok) return yield* new Validation({ message: result.refusal.message, userFacing: true });
  return result.output;
});

export const listMembersServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug })))
  .handler(({ context, data }) => runActor(context, Effect.all({
    members: ran(data.organizationSlug, "org_member_ls", {}),
    invites: ran(data.organizationSlug, "org_invite_ls", {}),
  })));

export const inviteMemberServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug, email: Schema.NullOr(Schema.String), role: Schema.Literals(["admin", "member"]) })))
  .handler(({ context, data }) => {
    const email = data.email?.trim();
    return runActor(context, ran(data.organizationSlug, "org_member_invite", email ? { role: data.role, email } : { role: data.role }));
  });

export const setMemberRoleServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug, member: Schema.String, role: MemberRole })))
  .handler(({ context, data }) => runActor(context, ran(data.organizationSlug, "org_member_role", { member: data.member, role: data.role })));

export const removeMemberServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug, member: Schema.String })))
  .handler(({ context, data }) => runActor(context, ran(data.organizationSlug, "org_member_rm", { member: data.member })));

export const revokeInviteServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug, invite: Schema.String })))
  .handler(({ context, data }) => runActor(context, ran(data.organizationSlug, "org_invite_revoke", { invite: data.invite })));

export const leaveOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug })))
  .handler(({ context, data }) => runActor(context, ran(data.organizationSlug, "org_leave", {})));

/** Anyone holding the link may see where it leads, signed in or not: the token is the secret. */
export const readInviteServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware])
  .validator(strictValidator(Schema.Struct({ token: Schema.String })))
  .handler(({ data }) => runActor({ signal: getRequest().signal }, readInvite(data.token)));

export const acceptInviteServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ token: Schema.String })))
  .handler(({ context, data }) => runActor(context, acceptInvite(data.token, context.actor.userId)));
