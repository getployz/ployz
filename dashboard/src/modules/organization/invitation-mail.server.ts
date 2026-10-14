import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import { user } from "#/modules/identity/tables";
import { queueMail } from "#/modules/mail/mail.inngest";
import { renderMail } from "#/modules/mail/layout";
import { type InviteMail, inviteMail } from "#/modules/mail/templates/organizations/invite";
import { organization } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

export const inviteMailKey = (invitationId: string) => `invitation-${invitationId}`;

export const emailInvite = Effect.fn("Invitation.email")(function* (
  invite: { readonly id: string; readonly email: string | null; readonly role: InviteMail["role"]; readonly expiresAt: Date },
  organizationId: string,
  inviterId: string,
  link: string,
) {
  const config = yield* AppConfig;
  if (config.mail === null || invite.email === null) return null;
  const { drizzle } = yield* Database;
  const [names] = yield* drizzle.select({ organization: organization.name, inviter: user.name })
    .from(organization).innerJoin(user, eq(user.id, inviterId))
    .where(eq(organization.id, organizationId)).limit(1);
  if (names === undefined) return null;
  const content = inviteMail({ to: invite.email, ...names, role: invite.role, link, expiresAt: invite.expiresAt });
  const rendered = yield* Effect.promise(() => renderMail(content, config.app.url.origin));
  return yield* queueMail({ to: invite.email, ...rendered }, inviteMailKey(invite.id)).pipe(
    Effect.as(invite.email),
    Effect.catchTag("InngestEventSendError", (error) =>
      Effect.logWarning("Invite mail was not queued; the dialog still shows the link.", error).pipe(Effect.as(null))),
  );
});
