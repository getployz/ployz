import "@tanstack/react-start/server-only";
import { and, eq, sql } from "drizzle-orm";
import { Effect } from "effect";
import { refusal, type StoreRefusal } from "#/lib/refusal";
import { type Caller, callerEnvironments } from "#/modules/identity/actor";
import type { ServiceStep } from "#/modules/proposals/proposal";
import { operationProposals } from "#/modules/proposals/tables";
import { AppConfig } from "#/server/config.server";
import { Database } from "#/server/database.server";

/** Where a person approves a presented proposal. */
export const approvalUrl = (appUrl: URL, organizationSlug: string, proposalId: string) =>
  new URL(`/cloud/${organizationSlug}/approve/${proposalId}`, appUrl).href;

const STOPPED = { stop: "stopped", restart: "restarted", start: "started" } as const;

/**
 * Present a stop or restart a person decides: an agent's, or a member's in a Protected Environment they can't protect.
 * The same ask from the same credential finds the one still waiting, under a lock so two at once make one. Answers the
 * refusal that names the link: nothing runs until someone approves it there.
 */
export const proposeServiceAct = Effect.fn("Proposals.proposeServiceAct")(function* (
  caller: Caller,
  place: { readonly environmentId: string; readonly project: string; readonly environment: string; readonly namespace: string },
  step: ServiceStep,
) {
  const database = yield* Database;
  const id = yield* database.transaction(Effect.gen(function* () {
    const { drizzle } = yield* Database;
    yield* drizzle.execute(sql`select pg_advisory_xact_lock(hashtext(${`service-proposal:${caller.organization.id}:${caller.credential.id}`}))`);
    const [again] = yield* drizzle.select({ id: operationProposals.id }).from(operationProposals).where(and(
      eq(operationProposals.organizationId, caller.organization.id),
      eq(operationProposals.environmentId, place.environmentId),
      eq(operationProposals.status, "presented"),
      eq(operationProposals.credentialKind, caller.credential.kind),
      eq(operationProposals.credentialId, caller.credential.id),
      sql`${operationProposals.steps} = ${JSON.stringify([step])}::jsonb`,
    )).limit(1);
    if (again !== undefined) return again.id;
    const [row] = yield* drizzle.insert(operationProposals).values({
      organizationId: caller.organization.id,
      environmentId: place.environmentId,
      project: place.project,
      environment: place.environment,
      namespace: place.namespace,
      requestedByUserId: caller.userId,
      credentialKind: caller.credential.kind,
      credentialId: caller.credential.id,
      callerEnvironments: callerEnvironments(caller) ?? null,
      steps: [step],
      status: "presented",
    }).returning({ id: operationProposals.id });
    return row?.id ?? (yield* Effect.die("an insert returned no row"));
  }));
  const { app } = yield* AppConfig;
  const url = approvalUrl(app.url, caller.organization.slug, id);
  return refusal("conflict", `Waiting for approval at ${url}. Nothing ${STOPPED[step.verb]} yet.`, { proposal_id: id, status: "presented", url }) satisfies StoreRefusal;
});
