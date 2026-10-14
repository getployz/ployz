import "@tanstack/react-start/server-only";
import { eq } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { user } from "#/modules/identity/tables";
import {
  DEFAULT_NOTIFICATION_CHANNEL,
  NOTIFICATION_TYPE_IDS,
  type NotificationChannel,
  type NotificationSettings,
  type NotificationType,
} from "#/modules/notifications/notification-types";
import { notificationPreference } from "#/modules/notifications/tables";
import type { UnsubscribeTarget } from "#/modules/notifications/unsubscribe-token.server";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

export const getNotificationSettings = Effect.fn("Notifications.getSettings")(function* (actor: Actor) {
  const config = yield* AppConfig;
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({ type: notificationPreference.type, channel: notificationPreference.channel })
    .from(notificationPreference)
    .where(eq(notificationPreference.userId, actor.userId));
  const chosen = new Map(rows.map((row) => [row.type, row.channel]));
  // SAFETY: the entries hold exactly one channel for each notification type.
  const channels = Object.fromEntries(
    NOTIFICATION_TYPE_IDS.map((type) => [type, chosen.get(type) ?? DEFAULT_NOTIFICATION_CHANNEL]),
  ) as Record<NotificationType, NotificationChannel>;
  return { mailOn: config.mail !== null, channels } satisfies NotificationSettings;
});

export const setNotificationChannel = Effect.fn("Notifications.setChannel")(function* (
  userId: string,
  type: NotificationType,
  channel: NotificationChannel,
) {
  const { drizzle } = yield* Database;
  yield* drizzle
    .insert(notificationPreference)
    .values({ userId, type, channel })
    .onConflictDoUpdate({
      target: [notificationPreference.userId, notificationPreference.type],
      set: { channel, updatedAt: new Date() },
    });
});

/** A deleted user's old link still answers, and writes nothing. */
export const unsubscribe = Effect.fn("Notifications.unsubscribe")(function* (target: UnsubscribeTarget) {
  const { drizzle } = yield* Database;
  const known = yield* drizzle.select({ id: user.id }).from(user).where(eq(user.id, target.userId)).limit(1);
  if (known.length > 0) yield* setNotificationChannel(target.userId, target.type, "none");
});
