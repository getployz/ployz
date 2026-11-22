import type { MachineId } from "@ployz/sdk";
import { jsonb, pgTable, primaryKey, text, timestamp, uuid } from "drizzle-orm/pg-core";
import { updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";

/**
 * What the mirror syncs saw of one mirror. The last pull that landed and the last that failed are kept apart, so a newer
 * success never hides an older failure; `matchedAt` is the newest time the mirror held the writer's data as of: a sync
 * that found it at the writer's newest, or the taking of a snapshot it landed.
 */
export const volumeMirrorStatus = pgTable(
  "volume_mirror_status",
  {
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    volumeId: text("volume_id").notNull(),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    okAt: timestamp("ok_at", { mode: "date", withTimezone: true }),
    okSnapshot: text("ok_snapshot"),
    matchedAt: timestamp("matched_at", { mode: "date", withTimezone: true }),
    failedAt: timestamp("failed_at", { mode: "date", withTimezone: true }),
    failedSnapshot: text("failed_snapshot"),
    failure: text("failure"),
    /** Unused: the Mirror approvals Cloud kept for this Mirror. */
    approvals: jsonb("approvals"),
    updatedAt,
  },
  (table) => [primaryKey({ columns: [table.volumeId, table.machineId] })],
);
