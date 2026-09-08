import { createdAt, type JsonObject, updatedAt } from "#/db/tables";
import { organization } from "#/modules/organization/tables";
import { sql } from "drizzle-orm";
import {
  check,
  integer,
  jsonb,
  pgTable,
  text,
  timestamp,
  unique,
  uuid,
} from "drizzle-orm/pg-core";

export type CoreOperationWatchCursorState =
  | "more"
  | "caught_up"
  | "terminal";

export type CoreOperationObservationState =
  | "active"
  | "core_terminal"
  | "cloud_timeout"
  | "cloud_cancelled";

export type StoredCoreOperationExpectedKind =
  | "deploy"
  | "cert"
  | "machine_add"
  | "machine_update"
  | "machine_lifecycle"
  | "core_replace"
  | "credential_grant"
  | "network_repair"
  | "service_restart"
  | "managed_dns_reconcile"
  | "ingress_configure"
  | "ingress_refresh"
  | "namespace_remove"
  | "volume_create"
  | "volume_remove";

export const coreOperationWatch = pgTable(
  "core_operation_watch",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    operationId: text("operation_id").notNull(),
    expectedKind: text("expected_kind")
      .notNull()
      .$type<StoredCoreOperationExpectedKind>(),
    startSequence: text("start_sequence").notNull(),
    nextSequence: text("next_sequence").notNull(),
    cursorState: text("cursor_state")
      .notNull()
      .$type<CoreOperationWatchCursorState>(),
    observationState: text("observation_state")
      .notNull()
      .default("active")
      .$type<CoreOperationObservationState>(),
    observationDetail: jsonb("observation_detail").$type<JsonObject | null>(),
    inngestRunId: text("inngest_run_id"),
    deadlineAt: timestamp("deadline_at", {
      mode: "date",
      withTimezone: true,
    }).notNull(),
    terminalAt: timestamp("terminal_at", {
      mode: "date",
      withTimezone: true,
    }),
    createdAt,
    updatedAt,
  },
  (table) => [
    unique().on(table.organizationId, table.operationId),
    check(
      "core_operation_watch_expected_kind_check",
      sql`${table.expectedKind} in ('deploy','cert','machine_add','machine_update','machine_lifecycle','core_replace','credential_grant','network_repair','service_restart','managed_dns_reconcile','ingress_configure','ingress_refresh','namespace_remove','volume_create','volume_remove')`,
    ),
    check(
      "core_operation_watch_cursor_state_check",
      sql`${table.cursorState} in ('more','caught_up','terminal')`,
    ),
    check(
      "core_operation_watch_observation_state_check",
      sql`${table.observationState} in ('active','core_terminal','cloud_timeout','cloud_cancelled')`,
    ),
  ],
);

export const coreOperationEvent = pgTable(
  "core_operation_event",
  {
    id: uuid("id").defaultRandom().primaryKey(),
    watchId: uuid("watch_id")
      .notNull()
      .references(() => coreOperationWatch.id, { onDelete: "cascade" }),
    sequence: text("sequence").notNull(),
    eventType: text("event_type").notNull(),
    schemaVersion: integer("schema_version").default(1).notNull(),
    payload: jsonb("payload").notNull().$type<JsonObject>(),
    createdAt,
  },
  (table) => [unique().on(table.watchId, table.sequence)],
);
