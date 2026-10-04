import { sql } from "drizzle-orm";
import { boolean, check, index, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { createdAt, type MachineId, sqlStringLiterals, updatedAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import {
  DEFAULT_SERVER_UPGRADE_SETTINGS,
  RELEASE_CHANNELS,
  type ReleaseChannel,
  UPGRADE_OUTCOMES,
  UPGRADE_TRIGGERS,
  type UpgradeOutcome,
  type UpgradeTrigger,
} from "#/modules/server-upgrade/server-upgrade";

/**
 * One Upgrade Cloud asked a Server for: history, read server-side (latest per Server), never into the Org Store.
 * One rollout run writes a row per Server, so the run ID repeats.
 */
export const serverUpgradeAttempt = pgTable(
  "server_upgrade_attempt",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    /** The Server's attempt ID, minted by Cloud; repeating a request with it returns the same attempt. */
    attemptId: text("attempt_id").notNull(),
    trigger: text("trigger").notNull().$type<UpgradeTrigger>(),
    /** Who clicked; automatic attempts have none. */
    requestedByUserId: uuid("requested_by_user_id").references(() => user.id, { onDelete: "set null" }),
    channel: text("channel").notNull().$type<ReleaseChannel>(),
    /** What the Server ran before; empty when its record predates daemon versions. */
    fromVersion: text("from_version").notNull(),
    /** The exact version the Server resolved, once it answered the request. */
    targetVersion: text("target_version"),
    outcome: text("outcome").default("running").notNull().$type<UpgradeOutcome>(),
    stage: text("stage"),
    error: text("error"),
    inngestRunId: text("inngest_run_id").notNull(),
    startedAt: timestamp("started_at", { mode: "date", withTimezone: true }).notNull(),
    endedAt: timestamp("ended_at", { mode: "date", withTimezone: true }),
  },
  (table) => [
    uniqueIndex("server_upgrade_attempt_attempt_uidx").on(table.organizationId, table.machineId, table.attemptId),
    index("server_upgrade_attempt_latest_idx").on(table.organizationId, table.machineId, table.startedAt),
    index("server_upgrade_attempt_run_idx").on(table.inngestRunId),
    check("server_upgrade_attempt_attempt_id_check", sql`${table.attemptId} ~ '^[0-9a-f]{32}$'`),
    check("server_upgrade_attempt_machine_id_check", sql`${table.machineId} ~ '^[0-9a-f]{32}$'`),
    check("server_upgrade_attempt_trigger_check", sql`${table.trigger} in (${sqlStringLiterals(UPGRADE_TRIGGERS)})`),
    check("server_upgrade_attempt_channel_check", sql`${table.channel} in (${sqlStringLiterals(RELEASE_CHANNELS)})`),
    check("server_upgrade_attempt_outcome_check", sql`${table.outcome} in (${sqlStringLiterals(UPGRADE_OUTCOMES)})`),
    check("server_upgrade_attempt_ended_check", sql`(${table.outcome} = 'running') = (${table.endedAt} is null)`),
    check("server_upgrade_attempt_error_check", sql`${table.error} is null or ${table.outcome} = 'failed'`),
  ],
);

/** The Organization's Server upgrade settings. No row reads as the defaults, so every Organization starts with manual upgrades. */
export const organizationServerUpgrades = pgTable("organization_server_upgrades", {
  organizationId: uuid("organization_id").primaryKey()
    .references(() => organization.id, { onDelete: "cascade" }),
  /** Off stops the hourly rollout; Upgrade still works. */
  automatic: boolean("automatic").default(DEFAULT_SERVER_UPGRADE_SETTINGS.automatic).notNull(),
  /** The Release Channel every Upgrade requests, manual or automatic. */
  channel: text("channel").default(DEFAULT_SERVER_UPGRADE_SETTINGS.channel).notNull().$type<ReleaseChannel>(),
  createdAt,
  updatedAt,
}, (table) => [
  check("organization_server_upgrades_channel_check", sql`${table.channel} in (${sqlStringLiterals(RELEASE_CHANNELS)})`),
]);
