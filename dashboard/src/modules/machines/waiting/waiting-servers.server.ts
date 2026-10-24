import "@tanstack/react-start/server-only";
import { and, asc, desc, eq, isNotNull, isNull, or, sql } from "drizzle-orm";
import { Effect, Option } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { organizationMachine } from "#/modules/machines/tables";
import { CHECKIN_WAIT_SECONDS, type StoredReport } from "#/modules/machines/waiting/checkin-wire";
import { type PortStates, portStates } from "#/modules/machines/waiting/probe";
import { decodeStoredProbe } from "#/modules/machines/waiting/probe.server";
import { fleetToken, serverEnrollment, waitingServer } from "#/modules/machines/waiting/tables";
import { requireAccess } from "#/modules/organization/access.server";
import { Database } from "#/server/database.server";
import { NotFound } from "#/server/public-error";

const OFFLINE_AFTER_MS = 3 * CHECKIN_WAIT_SECONDS * 1000;

/** A host stops waiting once an enrollment is under way for it or has made it a Server. */
export const stillWaiting = sql`not exists (select 1 from ${serverEnrollment} where ${serverEnrollment.waitingServerId} = ${waitingServer.id}
  and (${serverEnrollment.endedAt} is null or ${serverEnrollment.state} = 'joined'))`;

/**
 * A joined host's check-in loop stops on its next answer, so a check-in this long after its join ended comes from a host
 * that was reset or installed again while it kept its check-in key.
 */
const REJOINED_AFTER = sql`interval '5 minutes'`;

/** What blocks a host that checks in again while the Server it joined as is still listed. */
export function formerServerBlocker(name: string) {
  return {
    code: "former_server",
    message: `This server was ${name}, which is still listed. Forget ${name} to add it again: ployz server forget ${name}`,
  } as const;
}

export type WaitingServerStatus = "offline" | "blocked" | "preparing" | "waiting";

export type WaitingServerDetail = {
  readonly id: string;
  readonly report: StoredReport;
  readonly observedIp: string;
  readonly fleetToken: string;
  readonly status: WaitingServerStatus;
  readonly firstSeenAt: string;
  readonly lastCheckInAt: string;
  readonly probe: PortStates | null;
  /** The latest enroll attempt when it ended without joining. It stays until the next attempt. */
  readonly returned: WaitingReturn | null;
};

export type WaitingReturn = {
  readonly state: "failed" | "cancelled";
  readonly stage: string | null;
  readonly message: string | null;
};

type ListReport = Pick<StoredReport, "hostname" | "os" | "cpuCount" | "memoryTotalBytes" | "blockers"> & {
  readonly provider?: StoredReport["provider"];
  readonly prep?: StoredReport["prep"];
  readonly disks: ReadonlyArray<Pick<StoredReport["disks"][number], "role" | "sizeBytes">>;
  readonly zfs: { readonly kind: "eligible"; readonly buildMinutes?: number | undefined }
    | Extract<StoredReport["zfs"], { readonly kind: "ineligible" }>;
};

export type WaitingServerView = Omit<WaitingServerDetail, "report" | "probe"> & {
  readonly report: ListReport;
  readonly arrivalAt: string;
};

export function waitingServerStatus(report: StoredReport, lastCheckInAt: Date, now: Date): WaitingServerStatus {
  if (now.getTime() - lastCheckInAt.getTime() > OFFLINE_AFTER_MS) return "offline";
  if (report.blockers.length > 0) return "blocked";
  if (report.prep !== undefined && report.prep.docker !== "ready") return "preparing";
  return "waiting";
}

const listed = Effect.fn("WaitingServer.read")(function* (organizationId: string, id: string | null) {
  const { drizzle } = yield* Database;
  const latest = drizzle.select({ state: serverEnrollment.state, stage: serverEnrollment.stage, failure: serverEnrollment.failure, endedAt: serverEnrollment.endedAt })
    .from(serverEnrollment).where(eq(serverEnrollment.waitingServerId, waitingServer.id))
    .orderBy(desc(serverEnrollment.requestedAt), desc(serverEnrollment.id)).limit(1).as("latest_return");
  // The Server the host joined as, while Cloud still lists it.
  const former = drizzle.select({
    attemptId: sql<string>`${serverEnrollment.id}`.as("former_attempt_id"),
    machineId: sql<string>`${serverEnrollment.machineId}`.as("former_machine_id"),
    machineName: organizationMachine.machineName,
    endedAt: sql<Date>`${serverEnrollment.endedAt}`.as("former_ended_at"),
  })
    .from(serverEnrollment)
    .innerJoin(organizationMachine, and(
      eq(organizationMachine.organizationId, serverEnrollment.organizationId),
      eq(organizationMachine.machineId, serverEnrollment.machineId),
    ))
    .where(and(eq(serverEnrollment.waitingServerId, waitingServer.id), eq(serverEnrollment.state, "joined")))
    .orderBy(desc(serverEnrollment.endedAt)).limit(1).as("former_server");
  const noOpenEnrollment = sql`not exists (select 1 from ${serverEnrollment} where ${serverEnrollment.waitingServerId} = ${waitingServer.id}
    and ${serverEnrollment.endedAt} is null)`;
  const arrival = sql<Date>`coalesce(${latest.endedAt}, ${waitingServer.firstSeenAt})`;
  const rows = yield* drizzle
    .select({
      id: waitingServer.id,
      report: waitingServer.report,
      observedIp: waitingServer.observedIp,
      fleetToken: fleetToken.name,
      firstSeenAt: waitingServer.firstSeenAt,
      lastCheckInAt: waitingServer.lastCheckInAt,
      probe: id === null ? sql<null>`null` : waitingServer.probe,
      arrivalAt: arrival,
      returnState: latest.state,
      returnStage: latest.stage,
      returnFailure: latest.failure,
      formerAttemptId: former.attemptId,
      formerMachineId: former.machineId,
      formerName: former.machineName,
    })
    .from(waitingServer)
    .innerJoin(fleetToken, eq(fleetToken.id, waitingServer.fleetTokenId))
    .leftJoinLateral(latest, sql`true`)
    .leftJoinLateral(former, sql`true`)
    .where(and(
      eq(waitingServer.organizationId, organizationId),
      isNull(waitingServer.rejectedAt),
      or(stillWaiting, and(
        isNotNull(former.machineId),
        noOpenEnrollment,
        sql`${waitingServer.lastCheckInAt} > ${former.endedAt} + ${REJOINED_AFTER}`,
        // That Server itself, still checking in because it missed Cloud's answer to its join.
        sql`not (coalesce(${waitingServer.report}->'attempt'->>'id', '') = ${former.attemptId}::text
          and coalesce((${waitingServer.report}->'attempt'->>'failed')::boolean, false))`,
      )),
      id === null ? undefined : eq(waitingServer.id, id),
    ))
    .orderBy(asc(arrival), asc(waitingServer.firstSeenAt), asc(waitingServer.id));
  const now = new Date();
  return rows.map(({ arrivalAt, returnState, returnStage, returnFailure, formerAttemptId: _attempt, formerMachineId, formerName, ...listedRow }) => {
    const { attempt: _reported, ...reported } = listedRow.report;
    // Listed only once the host checked in long after it joined, so a Server still listed is one it no longer is.
    const row = {
      ...listedRow,
      report: formerMachineId === null ? reported : { ...reported, blockers: [formerServerBlocker(formerName ?? formerMachineId), ...reported.blockers] },
    };
    return {
      ...row,
      arrivalAt: new Date(arrivalAt).toISOString(),
      returned: returnState === "failed" || returnState === "cancelled" ? { state: returnState, stage: returnFailure?.stage ?? returnStage, message: returnFailure?.message ?? null } satisfies WaitingReturn : null,
      status: waitingServerStatus(row.report, row.lastCheckInAt, now),
      firstSeenAt: row.firstSeenAt.toISOString(),
      lastCheckInAt: row.lastCheckInAt.toISOString(),
      probe: Option.match(decodeStoredProbe(row.probe), { onNone: () => null, onSome: (probe) => portStates(probe, now) }),
    };
  });
});

export const listWaitingServers = Effect.fn("WaitingServer.list")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:read");
  const rows = yield* listed(organization.id, null);
  return rows.map(({ probe: _probe, report, ...row }): WaitingServerView => ({
    ...row,
    report: {
      hostname: report.hostname, os: report.os, provider: report.provider,
      cpuCount: report.cpuCount, memoryTotalBytes: report.memoryTotalBytes,
      prep: report.prep, blockers: report.blockers.slice(0, 1),
      disks: report.disks.map(({ role, sizeBytes }) => ({ role, sizeBytes })),
      zfs: report.zfs.kind === "ineligible" ? report.zfs : {
        kind: "eligible",
        buildMinutes: report.zfs.buildMinutes,
      },
    },
  }));
});

export const showWaitingServer = Effect.fn("WaitingServer.show")(function* (
  actor: Actor,
  input: { readonly organizationSlug: string; readonly id: string },
) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "servers:read");
  const [row] = yield* listed(organization.id, input.id);
  if (row === undefined) return yield* new NotFound({ message: "No such waiting server." });
  return row;
});
