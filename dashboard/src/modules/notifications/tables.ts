import { sql } from "drizzle-orm";
import { check, pgTable, primaryKey, text, uuid } from "drizzle-orm/pg-core";
import { sqlStringLiterals, updatedAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import {
  NOTIFICATION_CHANNELS,
  NOTIFICATION_TYPE_IDS,
  type NotificationChannel,
  type NotificationType,
} from "#/modules/notifications/notification-types";

/** A user's choice for one notification type, across every Organization they belong to. No row means Email. */
export const notificationPreference = pgTable(
  "notification_preference",
  {
    userId: uuid("user_id")
      .notNull()
      .references(() => user.id, { onDelete: "cascade" }),
    type: text("type").notNull().$type<NotificationType>(),
    channel: text("channel").notNull().$type<NotificationChannel>(),
    updatedAt,
  },
  (table) => [
    primaryKey({ columns: [table.userId, table.type] }),
    check("notification_preference_type_check", sql`${table.type} in (${sqlStringLiterals(NOTIFICATION_TYPE_IDS)})`),
    check("notification_preference_channel_check", sql`${table.channel} in (${sqlStringLiterals(NOTIFICATION_CHANNELS)})`),
  ],
);
