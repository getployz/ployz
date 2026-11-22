import { index, integer, pgTable, text, timestamp, uuid } from "drizzle-orm/pg-core";

export const checkinRateLimit = pgTable("checkin_rate_limit", {
  key: text("key").primaryKey(),
  windowStart: timestamp("window_start", { mode: "date", withTimezone: true }).notNull(),
  count: integer("count").notNull(),
});

export const rateLimitEvent = pgTable(
  "rate_limit_event",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    key: text("key").notNull(),
    at: timestamp("at", { mode: "date", withTimezone: true }).notNull().defaultNow(),
  },
  (table) => [index("rate_limit_event_key_at_idx").on(table.key, table.at), index("rate_limit_event_at_idx").on(table.at)],
);
