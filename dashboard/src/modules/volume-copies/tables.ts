import { index, jsonb, pgTable, text, uuid } from "drizzle-orm/pg-core";
import { createdAt, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";
import type { SnapshotPolicy } from "./snapshot-schedule";

/** A Volume's automatic snapshots; a Volume without a row takes none. */
export const volumeSnapshotPolicy = pgTable(
  "volume_snapshot_policy",
  {
    volumeId: text("volume_id").primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: text("environment_id").notNull(),
    policy: jsonb("policy").$type<SnapshotPolicy>().notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [index("volume_snapshot_policy_environment_idx").on(table.organizationId, table.environmentId)],
);
