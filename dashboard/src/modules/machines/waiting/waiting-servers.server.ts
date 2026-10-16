import "@tanstack/react-start/server-only";
import { and, asc, eq, isNull } from "drizzle-orm";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { CHECKIN_WAIT_SECONDS, type StoredReport } from "#/modules/machines/waiting/checkin-wire";
import { fleetToken, waitingServer } from "#/modules/machines/waiting/tables";
import { requireAccess } from "#/modules/organization/access.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

const OFFLINE_AFTER_MS = 3 * CHECKIN_WAIT_SECONDS * 1000;

/** Read at display time, never stored. Rejected rows are tombstones and never listed. */
export type WaitingServerStatus = "offline" | "blocked" | "preparing" | "waiting";

export type WaitingServerView = {
  readonly id: string;
  readonly report: StoredReport;
  readonly observedIp: string;
  readonly fleetToken: string;
  readonly status: WaitingServerStatus;
  readonly firstSeenAt: string;
  readonly lastCheckInAt: string;
};

export function waitingServerStatus(report: StoredReport, lastCheckInAt: Date, now: Date): WaitingServerStatus {
  if (now.getTime() - lastCheckInAt.getTime() > OFFLINE_AFTER_MS) return "offline";
  if (report.blockers.length > 0) return "blocked";
  if (report.prep !== undefined && report.prep.docker !== "ready") return "preparing";
  return "waiting";
}

const listed = Effect.fn("WaitingServer.read")(function* (organizationId: string, id: string | null) {
  const { drizzle } = yield* Database;
  const rows = yield* drizzle
    .select({
      id: waitingServer.id,
      report: waitingServer.report,
      observedIp: waitingServer.observedIp,
      fleetToken: fleetToken.name,
      firstSeenAt: waitingServer.firstSeenAt,
      lastCheckInAt: waitingServer.lastCheckInAt,
    })
    .from(waitingServer)
    .innerJoin(fleetToken, eq(fleetToken.id, waitingServer.fleetTokenId))
    .where(and(
      eq(waitingServer.organizationId, organizationId),
      isNull(waitingServer.rejectedAt),
      id === null ? undefined : eq(waitingServer.id, id),
    ))
    .orderBy(asc(waitingServer.firstSeenAt), asc(waitingServer.id));
  const now = new Date();
  return rows.map((row): WaitingServerView => ({
    ...row,
    status: waitingServerStatus(row.report, row.lastCheckInAt, now),
    firstSeenAt: row.firstSeenAt.toISOString(),
    lastCheckInAt: row.lastCheckInAt.toISOString(),
  }));
});

/** The Organization's Waiting Servers, oldest first, so new arrivals append. */
export const listWaitingServers = Effect.fn("WaitingServer.list")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  return yield* listed(organization.id, null);
});

export const showWaitingServer = Effect.fn("WaitingServer.show")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly id: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const [row] = yield* listed(organization.id, input.id);
  if (row === undefined) return yield* new NotFound({ message: "No such waiting server." });
  return row;
});
