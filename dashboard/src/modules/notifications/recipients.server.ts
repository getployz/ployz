import "@tanstack/react-start/server-only";
import { and, eq, isNull, or } from "drizzle-orm";
import { Effect, Redacted } from "effect";
import { member, user } from "#/modules/identity/tables";
import { renderMail, type MailContent } from "#/modules/mail/layout";
import { queueMail } from "#/modules/mail/mail.inngest";
import { NOTIFICATION_TYPES, type NotificationType } from "#/modules/notifications/notification-types";
import { notificationPreference } from "#/modules/notifications/tables";
import { signUnsubscribeToken } from "#/modules/notifications/unsubscribe-token.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

/** Every member of the Organization, minus those who set `type` to None. */
export const resolveRecipients = Effect.fn("Notifications.resolveRecipients")(function* (
  organizationId: string,
  type: NotificationType,
) {
  const { drizzle } = yield* Database;
  return yield* drizzle
    .select({ userId: user.id, email: user.email })
    .from(member)
    .innerJoin(user, eq(user.id, member.userId))
    .leftJoin(notificationPreference, and(eq(notificationPreference.userId, member.userId), eq(notificationPreference.type, type)))
    .where(and(eq(member.organizationId, organizationId), or(isNull(notificationPreference.channel), eq(notificationPreference.channel, "email"))));
});

export type OrganizationMail = {
  readonly organizationId: string;
  readonly type: NotificationType;
  /** Stable for the event being told, so a retried caller queues each recipient's mail once. */
  readonly key: string;
  readonly content: Omit<MailContent, "unsubscribe">;
};

/** Render `content` once per recipient, each with their own unsubscribe link, and queue it keyed `key:userId`. */
export const queueOrganizationMail = Effect.fn("Notifications.queueOrganizationMail")(function* (mail: OrganizationMail) {
  const config = yield* AppConfig;
  const origin = config.app.url.origin;
  const recipients = yield* resolveRecipients(mail.organizationId, mail.type);
  yield* Effect.forEach(recipients, (recipient) =>
    Effect.gen(function* () {
      const token = signUnsubscribeToken(Redacted.value(config.encryptionSecret), { userId: recipient.userId, type: mail.type });
      const url = `${origin}/unsubscribe?token=${token}`;
      const unsubscribe = { label: NOTIFICATION_TYPES[mail.type].unsubscribe, url };
      const rendered = yield* Effect.promise(() => renderMail({ ...mail.content, unsubscribe }, origin));
      const headers = { "List-Unsubscribe": `<${url}>`, "List-Unsubscribe-Post": "List-Unsubscribe=One-Click" };
      yield* queueMail({ to: recipient.email, ...rendered, headers }, `${mail.key}:${recipient.userId}`);
    }),
  );
  return recipients.length;
});
