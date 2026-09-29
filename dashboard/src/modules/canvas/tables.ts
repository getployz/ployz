import { index, integer, pgTable, text, unique, uuid } from "drizzle-orm/pg-core";
import { createdAt, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";

/** Where the canvas draws a Config Store Service or Volume: presentation Cloud keeps, keyed by the node's id. */
export const environmentCanvasNodePosition = pgTable(
  "environment_canvas_node_position",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    // No foreign key: an Environment lives only in the Config Store's tables.
    environmentId: uuid("environment_id").notNull(),
    resourceType: text("resource_type").notNull(),
    resourceId: uuid("resource_id").notNull(),
    x: integer("x").notNull(),
    y: integer("y").notNull(),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.environmentId, table.resourceType, table.resourceId),
    index("environment_canvas_node_position_environment_id_idx").on(
      table.environmentId,
    ),
    index("environment_canvas_node_position_organization_id_idx").on(
      table.organizationId,
    ),
    index("environment_canvas_node_position_resource_lookup_idx").on(
      table.resourceType,
      table.resourceId,
    ),
  ],
);
