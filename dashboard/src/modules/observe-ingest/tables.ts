import { sql } from "drizzle-orm";
import { bigint, boolean, check, foreignKey, index, pgTable, primaryKey, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { type MachineId, updatedAt } from "#/db/tables";
import { organizationMachine } from "#/modules/machines/tables";

const timestamptz = (name: string) => timestamp(name, { mode: "date", withTimezone: true });

/** The streams a Machine pushes. A stream outside this list answers 404 so the Machine keeps it buffered. */
export const OBSERVE_STREAMS = ["crash"] as const;
export type ObserveStream = (typeof OBSERVE_STREAMS)[number];

/** The highest seq Cloud has taken from one Machine's stream. A push at or below it is a replay. */
export const observeIngestCursor = pgTable(
  "observe_ingest_cursor",
  {
    organizationId: uuid("organization_id").notNull(),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    stream: text("stream").notNull().$type<ObserveStream>(),
    seq: bigint("seq", { mode: "bigint" }).notNull(),
    updatedAt,
  },
  (table) => [
    primaryKey({ columns: [table.organizationId, table.machineId, table.stream] }),
    foreignKey({
      columns: [table.organizationId, table.machineId],
      foreignColumns: [organizationMachine.organizationId, organizationMachine.machineId],
      name: "observe_ingest_cursor_machine_fk",
    }).onDelete("cascade"),
    check("observe_ingest_cursor_stream_check", sql`${table.stream} in ('crash')`),
  ],
);

/** One container that crashed on a Server, as its Machine reported it. Mailed once, kept 30 days. */
export const crashEvent = pgTable(
  "crash_event",
  {
    id: uuid("id").primaryKey().defaultRandom(),
    organizationId: uuid("organization_id").notNull(),
    machineId: text("machine_id").notNull().$type<MachineId>(),
    seq: bigint("seq", { mode: "bigint" }).notNull(),
    // The Store node id, the same id the dashboard's service route takes.
    serviceId: text("service_id").notNull(),
    // Qualified as the marker carries it, `<namespace>/<runtime name>`.
    serviceName: text("service_name").notNull(),
    replica: text("replica").notNull(),
    containerId: text("container_id").notNull(),
    deploymentId: text("deployment_id"),
    exitCode: bigint("exit_code", { mode: "number" }),
    oom: boolean("oom").notNull(),
    occurredAt: timestamptz("occurred_at").notNull(),
    receivedAt: timestamptz("received_at").defaultNow().notNull(),
    mailedAt: timestamptz("mailed_at"),
  },
  (table) => [
    uniqueIndex("crash_event_machine_seq_idx").on(table.organizationId, table.machineId, table.seq),
    index("crash_event_service_idx").on(table.organizationId, table.serviceId, table.mailedAt),
    index("crash_event_occurred_idx").on(table.occurredAt),
    foreignKey({
      columns: [table.organizationId, table.machineId],
      foreignColumns: [organizationMachine.organizationId, organizationMachine.machineId],
      name: "crash_event_machine_fk",
    }).onDelete("cascade"),
  ],
);
