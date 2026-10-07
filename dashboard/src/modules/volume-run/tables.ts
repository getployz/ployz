import { sql } from "drizzle-orm";
import { bigint, boolean, check, index, jsonb, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { createdAt, sqlStringLiterals, updatedAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import {
  type AnyVolumeRunArgs,
  VOLUME_RUN_KINDS,
  VOLUME_RUN_STATES,
  type VolumeRunKind,
  type VolumeRunState,
} from "#/modules/volume-run/volume-run";

export const volumeRun = pgTable(
  "volume_run",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: text("environment_id").notNull(),
    volumeId: text("volume_id").notNull(),
    volumeName: text("volume_name").notNull(),
    dockerVolume: text("docker_volume").notNull(),
    kind: text("kind").notNull().$type<VolumeRunKind>(),
    args: jsonb("args").notNull().$type<AnyVolumeRunArgs>(),
    orphan: boolean("orphan").default(false).notNull(),
    // 0 for a Volume without a maximum (a Docker Volume).
    refquotaBytes: bigint("refquota_bytes", { mode: "number" }).default(0).notNull(),
    state: text("state").default("requested").notNull().$type<VolumeRunState>(),
    lease: bigint("lease", { mode: "number" }),
    inngestRunId: text("inngest_run_id"),
    requestedByUserId: uuid("requested_by_user_id").references(() => user.id, { onDelete: "set null" }),
    message: text("message"),
    createdAt,
    updatedAt,
    finishedAt: timestamp("finished_at", { mode: "date", withTimezone: true }),
  },
  (table) => [
    uniqueIndex("volume_run_one_active_idx")
      .on(table.volumeId)
      .where(sql`${table.state} in ('requested', 'running')`),
    index("volume_run_latest_idx").on(table.organizationId, table.volumeId, table.createdAt.desc()),
    check("volume_run_kind_check", sql`${table.kind} in (${sqlStringLiterals(VOLUME_RUN_KINDS)})`),
    check("volume_run_state_check", sql`${table.state} in (${sqlStringLiterals(VOLUME_RUN_STATES)})`),
    check("volume_run_message_check", sql`${table.message} is null or length(${table.message}) between 1 and 1024`),
  ],
);
