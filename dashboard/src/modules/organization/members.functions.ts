import { createServerFn } from "@tanstack/react-start";
import { getRequest } from "@tanstack/react-start/server";
import type { JsonValue } from "@ployz/sdk";
import { Effect, Schema } from "effect";
import { type CommandName, runCommand } from "#/modules/commands/commands.server";
import { type Caller, callerCan } from "#/modules/identity/actor";
import { acceptInvite, readInvite } from "#/modules/organization/invitation.server";
import { InviteRole, MemberRole } from "#/modules/organization/permissions";
import { Auth } from "#/modules/identity/auth.server";
import { AppConfig } from "#/server/config.server";
import { Validation } from "#/server/public-error";
import { authenticated, callerMiddleware, OrganizationSlugInput, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const Slug = Schema.NonEmptyString;

const ranResult = <N extends CommandName>(caller: Caller, name: N, input: JsonValue) => Effect.gen(function* () {
  const result = yield* runCommand(caller, name, input, { approvalId: null, key: null });
  if (!result.ok) return yield* new Validation({ message: result.refusal.message, userFacing: true });
  return result;
});

const ranAs = <N extends CommandName>(caller: Caller, name: N, input: JsonValue) =>
  ranResult(caller, name, input).pipe(Effect.map((result) => result.output));

/**
 * Pending invites name who was invited, so only owners and admins see them. `support` marks a support visit, which has
 * no row of its own.
 */
export const listMembersServerFn = createServerFn({ method: "GET" })
  .middleware([callerMiddleware(OrganizationSlugInput)])
  .handler(({ context }) => {
    const { caller } = context;
    return runActor(context, Effect.all({
      members: ranAs(caller, "org_member_ls", {}),
      invites: callerCan(caller, "members:manage") ? ranAs(caller, "org_invite_ls", {}) : Effect.succeed([]),
      mail: Effect.map(AppConfig, (config) => config.mail !== null),
      support: Effect.succeed(caller.support !== null),
    }));
  });

export const inviteMemberServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(Schema.Struct({ organizationSlug: Slug, email: Schema.NullOr(Schema.String), role: InviteRole }))])
  .handler(({ context, data }) => {
    const email = data.email?.trim();
    const input: JsonValue = email ? { role: data.role, email } : { role: data.role };
    return runActor(context, ranResult(context.caller, "org_member_invite", input).pipe(
      Effect.map(({ output, message }) => ({ ...output, message })),
    ));
  });

export const setMemberRoleServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(Schema.Struct({ organizationSlug: Slug, member: Schema.String, role: MemberRole }))])
  .handler(({ context, data }) => runActor(context, ranAs(context.caller, "org_member_role", { member: data.member, role: data.role })));

export const removeMemberServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(Schema.Struct({ organizationSlug: Slug, member: Schema.String }))])
  .handler(({ context, data }) => runActor(context, ranAs(context.caller, "org_member_rm", { member: data.member })));

export const revokeInviteServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(Schema.Struct({ organizationSlug: Slug, invite: Schema.String }))])
  .handler(({ context, data }) => runActor(context, ranAs(context.caller, "org_invite_revoke", { invite: data.invite })));

export const leaveOrganizationServerFn = createServerFn({ method: "POST" })
  .middleware([callerMiddleware(OrganizationSlugInput)])
  .handler(({ context }) => runActor(context, ranAs(context.caller, "org_leave", { organization: context.caller.organization.id })));

/** Anyone holding the link may see where it leads, signed in or not: the token is the secret. */
export const readInviteServerFn = createServerFn({ method: "POST" })
  .middleware([publicErrorMiddleware])
  .validator(strictValidator(Schema.Struct({ token: Schema.String })))
  .handler(({ data }) => {
    const { headers, signal } = getRequest();
    return runActor({ signal }, Effect.flatMap(Auth, (auth) => auth.getSession(headers)).pipe(Effect.flatMap((session) => readInvite(data.token, session?.user.id ?? null))));
  });

export const acceptInviteServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(Schema.Struct({ token: Schema.String })))
  .handler(({ context, data }) => runActor(context, acceptInvite(data.token, context.actor.userId)));
