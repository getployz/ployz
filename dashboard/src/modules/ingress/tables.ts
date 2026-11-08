import { sql } from "drizzle-orm";
import { check, index, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { createdAt, sqlStringLiterals, updatedAt } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { DOORS_STAGES, DOORS_STATES, DOORS_WANTS, type DoorsStage, type DoorsState, type DoorsWant } from "#/modules/ingress/doors";

const timestamptz = (name: string) => timestamp(name, { mode: "date", withTimezone: true });

/**
 * The Organization's Cloudflare tunnel as setup left it, and whether the Cluster Domain's source is its zone. It holds no
 * Cloudflare credential: the API token setup used is forgotten, and the tunnel's own token is sealed in the Cluster.
 * `set_up`: the tunnel runs beside the public ports and nothing moved. `active`: generated names sit under `zone`.
 * `left`: the Cluster went back to Public, and the tunnel and wildcard record are the user's to delete in Cloudflare.
 */
export const organizationCloudflareIngress = pgTable("organization_cloudflare_ingress", {
  organizationId: uuid("organization_id").primaryKey().references(() => organization.id, { onDelete: "cascade" }),
  zone: text("zone").notNull(),
  tunnelId: text("tunnel_id").notNull(),
  tunnelName: text("tunnel_name").notNull(),
  state: text("state").notNull().$type<"set_up" | "active" | "left">(),
  createdAt,
  updatedAt,
}, (table) => [
  check("organization_cloudflare_ingress_state_check", sql`${table.state} in ('set_up', 'active', 'left')`),
  check("organization_cloudflare_ingress_zone_check", sql`${table.zone} ~ '^[a-z0-9.-]+\\.[a-z0-9-]+$'`),
]);

/**
 * One switch of the Cluster's web traffic between the Public Door and the Tunnel Door. The request writes `pending`
 * under the id the tab minted; the run binds itself, claims it `running` right before it asks the Engine, moves
 * `stage` as the switch goes, and ends it `finished` or `failed` with why.
 */
export const ingressDoorsAttempt = pgTable("ingress_doors_attempt", {
  id: uuid("id").primaryKey(),
  organizationId: uuid("organization_id").notNull().references(() => organization.id, { onDelete: "cascade" }),
  requestedByUserId: uuid("requested_by_user_id").references(() => user.id, { onDelete: "set null" }),
  want: text("want").notNull().$type<DoorsWant>(),
  state: text("state").default("pending").notNull().$type<DoorsState>(),
  stage: text("stage").$type<DoorsStage>(),
  inngestRunId: text("inngest_run_id"),
  failure: text("failure"),
  requestedAt: timestamptz("requested_at").defaultNow().notNull(),
  startedAt: timestamptz("started_at"),
  endedAt: timestamptz("ended_at"),
}, (table) => [
  uniqueIndex("ingress_doors_attempt_one_active_idx").on(table.organizationId).where(sql`${table.state} in ('pending', 'running')`),
  uniqueIndex("ingress_doors_attempt_run_uidx").on(table.inngestRunId).where(sql`${table.inngestRunId} is not null`),
  index("ingress_doors_attempt_latest_idx").on(table.organizationId, table.requestedAt),
  check("ingress_doors_attempt_want_check", sql`${table.want} in (${sqlStringLiterals(DOORS_WANTS)})`),
  check("ingress_doors_attempt_state_check", sql`${table.state} in (${sqlStringLiterals(DOORS_STATES)})`),
  check("ingress_doors_attempt_stage_check", sql`${table.stage} is null or ${table.stage} in (${sqlStringLiterals(DOORS_STAGES)})`),
  check(
    "ingress_doors_attempt_failure_check",
    sql`(${table.state} = 'failed') = (${table.failure} is not null)
      and (${table.failure} is null or length(${table.failure}) between 1 and 1024)`,
  ),
  check("ingress_doors_attempt_run_check", sql`${table.inngestRunId} is null or length(${table.inngestRunId}) between 1 and 255`),
]);
