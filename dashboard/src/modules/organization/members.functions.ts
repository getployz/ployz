import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import type { JsonValue } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { type CommandName, runCommand } from "#/modules/commands/commands.server";
import type { Caller } from "#/modules/identity/actor";
import { dashboardCaller } from "#/modules/identity/caller.server";
import { acceptInvite, readInvite } from "#/modules/organization/invitation.server";
import { manages } from "#/modules/organization/members.commands.server";
import { AppConfig } from "#/server/config.server";
import { Validation } from "#/server/public-error";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Slug = Schema.NonEmptyString;
const MemberRole = Schema.Literals(["owner", "admin", "member"]);

const ranResult = <N extends CommandName>(caller: Caller, name: N, input: JsonValue) => Effect.gen(function* () {
  const result = yield* runCommand(caller, name, input, { approvalId: null, key: null });
  if (!result.ok) return yield* new Validation({ message: result.refusal.message, userFacing: true });
  return result;
});

const ranAs = <N extends CommandName>(caller: Caller, name: N, input: JsonValue) =>
  ranResult(caller, name, input).pipe(Effect.map((result) => result.output));

/** Takes the headers because getRequest() is only the caller's request before the Effect first yields. */
const ran = <N extends CommandName>(headers: Headers, organizationSlug: string, name: N, input: JsonValue) =>
  dashboardCaller(headers, organizationSlug).pipe(Effect.flatMap((caller) => ranAs(caller, name, input)));

/** Pending invites name who was invited, so only owners and admins see them. */
export const listMembersServerFn = createServerFn({ method: "GET" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug })))
  .handler(({ context, data }) => {
    const { headers } = getRequest();
    return runActor(context, dashboardCaller(headers, data.organizationSlug).pipe(Effect.flatMap((caller) => Effect.all({
      members: ranAs(caller, "org_member_ls", {}),
      invites: manages(caller) ? ranAs(caller, "org_invite_ls", {}) : Effect.succeed([]),
      mail: Effect.map(AppConfig, (config) => config.mail !== null),
    }))));
  });

export const inviteMemberServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug, email: Schema.NullOr(Schema.String), role: Schema.Literals(["admin", "member"]) })))
  .handler(({ context, data }) => {
    const email = data.email?.trim();
    const input: JsonValue = email ? { role: data.role, email } : { role: data.role };
    return runActor(context, dashboardCaller(getRequest().headers, data.organizationSlug).pipe(
      Effect.flatMap((caller) => ranResult(caller, "org_member_invite", input)),
      Effect.map(({ output, message }) => ({ ...output, message })),
    ));
  });

export const setMemberRoleServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug, member: Schema.String, role: MemberRole })))
  .handler(({ context, data }) => runActor(context, ran(getRequest().headers, data.organizationSlug, "org_member_role", { member: data.member, role: data.role })));

export const removeMemberServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug, member: Schema.String })))
  .handler(({ context, data }) => runActor(context, ran(getRequest().headers, data.organizationSlug, "org_member_rm", { member: data.member })));

export const revokeInviteServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug, invite: Schema.String })))
  .handler(({ context, data }) => runActor(context, ran(getRequest().headers, data.organizationSlug, "org_invite_revoke", { invite: data.invite })));

export const leaveOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ organizationSlug: Slug })))
  .handler(({ context, data }) => runActor(context, ran(getRequest().headers, data.organizationSlug, "org_leave", {})));

/** Anyone holding the link may see where it leads, signed in or not: the token is the secret. */
export const readInviteServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware])
  .validator(strictValidator(Schema.Struct({ token: Schema.String })))
  .handler(({ data }) => runActor({ signal: getRequest().signal }, readInvite(data.token)));

export const acceptInviteServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware, actorMiddleware])
  .validator(strictValidator(Schema.Struct({ token: Schema.String })))
  .handler(({ context, data }) => runActor(context, acceptInvite(data.token, context.actor.userId)));
