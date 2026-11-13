import type { ServerRef, ServiceVerb, Signed } from "@ployz/sdk";
import { sql } from "drizzle-orm";
import { check, index, integer, jsonb, pgTable, text, timestamp, uniqueIndex, uuid } from "drizzle-orm/pg-core";
import { sqlStringLiterals } from "#/db/tables";
import { user } from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import { operationProposals } from "#/modules/proposals/tables";
import {
  SERVICE_ACT_STATES,
  SERVICE_ACT_VERBS,
  type ServiceActBefore,
  type ServiceActLeg,
  type ServiceActState,
} from "#/modules/service-act/service-act";

export const serviceAct = pgTable(
  "service_act",
  {
    id: uuid("id").primaryKey(),
    organizationId: uuid("organization_id")
      .notNull()
      .references(() => organization.id, { onDelete: "cascade" }),
    environmentId: text("environment_id").notNull(),
    service: text("service").notNull(),
    namespace: text("namespace").notNull(),
    runtime: text("runtime").notNull(),
    verb: text("verb").notNull().$type<ServiceVerb>(),
    only: jsonb("only").$type<ServerRef | null>(),
    digest: text("digest").notNull(),
    state: text("state").default("accepted").notNull().$type<ServiceActState>(),
    before: jsonb("before").$type<ServiceActBefore | null>(),
    legs: jsonb("legs").default([]).notNull().$type<ServiceActLeg[]>(),
    /** The browser's approval for each Container it named, forwarded unchanged to a Server that enforces. Public material only. */
    approvals: jsonb("approvals").$type<Readonly<Record<string, Signed>> | null>(),
    /** The proposal this run is a step of, and which step, so its end starts the next. */
    proposalId: uuid("proposal_id").references(() => operationProposals.id, { onDelete: "set null" }),
    proposalStep: integer("proposal_step"),
    reason: text("reason"),
    inngestRunId: text("inngest_run_id"),
    requestedByUserId: uuid("requested_by_user_id").references(() => user.id, { onDelete: "set null" }),
    requestedAt: timestamp("requested_at", { mode: "date", withTimezone: true }).defaultNow().notNull(),
    startedAt: timestamp("started_at", { mode: "date", withTimezone: true }),
    endedAt: timestamp("ended_at", { mode: "date", withTimezone: true }),
  },
  (table) => [
    uniqueIndex("service_act_one_active_idx")
      .on(table.organizationId, table.environmentId, table.runtime)
      .where(sql`${table.state} in ('accepted', 'running')`),
    uniqueIndex("service_act_run_uidx").on(table.inngestRunId).where(sql`${table.inngestRunId} is not null`),
    index("service_act_latest_idx").on(table.organizationId, table.environmentId, table.service, table.requestedAt.desc()),
    check("service_act_state_check", sql`${table.state} in (${sqlStringLiterals(SERVICE_ACT_STATES)})`),
    check("service_act_verb_check", sql`${table.verb} in (${sqlStringLiterals(SERVICE_ACT_VERBS)})`),
    check("service_act_legs_check", sql`jsonb_typeof(${table.legs}) = 'array'`),
    check("service_act_approvals_check", sql`${table.approvals} is null or jsonb_typeof(${table.approvals}) = 'object'`),
    check("service_act_proposal_check", sql`(${table.proposalId} is null) = (${table.proposalStep} is null)`),
    check("service_act_reason_check", sql`${table.reason} is null or length(${table.reason}) between 1 and 1024`),
    check(
      "service_act_state_shape_check",
      sql`(
        (${table.state} = 'accepted' and ${table.startedAt} is null and ${table.endedAt} is null)
        or (${table.state} = 'running' and ${table.inngestRunId} is not null
          and ${table.startedAt} is not null and ${table.endedAt} is null and ${table.before} is not null)
        or (${table.state} in ('done', 'failed', 'unknown') and ${table.endedAt} is not null)
      )`,
    ),
  ],
);
