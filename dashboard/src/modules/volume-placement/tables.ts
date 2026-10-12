import { index, jsonb, pgTable, text, uuid } from "drizzle-orm/pg-core";
import type { MachineId } from "@ployz/sdk";
import { createdAt, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";
import type { StoredPlacement } from "#/modules/volume-placement/volume-placement";

/** Which copies of a Volume are mirrors, and the placement the user staged for the next Deploy. */
export const volumePlacement = pgTable(
  "volume_placement",
  {
    volumeId: text("volume_id").primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: text("environment_id").notNull(),
    mirrors: jsonb("mirrors").$type<MachineId[]>(),
    staged: jsonb("staged").$type<StoredPlacement>(),
    deploymentId: text("deployment_id"),
    createdAt,
    updatedAt,
  },
  (table) => [index("volume_placement_environment_idx").on(table.organizationId, table.environmentId)],
);
