import { Effect, Schema } from "effect";
import { Uuid } from "#/lib/schema";
import {
  cancelServerEnrollment,
  previewServerEnrollment,
  readServerEnrollments,
  type ServerEnrollmentView,
  startServerEnrollment,
} from "#/modules/machines/waiting/enroll.server";
import { openProbeSelection } from "#/modules/machines/waiting/probe.server";
import { rejectWaitingServers } from "#/modules/machines/waiting/reject.server";
import type { EnrollInput } from "#/modules/machines/waiting/server-enrollment";
import { listWaitingServers, showWaitingServer, type WaitingServerView } from "#/modules/machines/waiting/waiting-servers.server";
import { decodeBody, type CliRoutes } from "#/server/cli-call";
import { NotFound } from "#/server/public-error";

const WaitingServerIds = Schema.Struct({ ids: Schema.Array(Uuid).check(Schema.isMinLength(1), Schema.isMaxLength(1000)) });

const EnrollServers = Schema.Struct({
  ids: Schema.Array(Uuid).check(Schema.isMinLength(1), Schema.isMaxLength(1000)),
  first: Schema.optionalKey(Uuid),
  storage: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("zfs"), use_spare_disk: Schema.Boolean, fallback_to_docker: Schema.Boolean }),
    Schema.Struct({ kind: Schema.Literal("none") }),
  ]),
  names: Schema.Union([
    Schema.Struct({ kind: Schema.Literal("hostnames") }),
    Schema.Struct({ kind: Schema.Literal("pattern"), pattern: Schema.Trim.check(Schema.isMinLength(1), Schema.isMaxLength(63)) }),
  ]),
  runs: Schema.Struct({ services: Schema.Boolean, ingress: Schema.Boolean, builds: Schema.Boolean }),
});

const ENROLL_REFUSAL = "Enrolling takes 1 to 1000 waiting server ids, storage, names and what the servers run.";

function enrollInput(body: typeof EnrollServers.Type): EnrollInput {
  const input: EnrollInput = {
    ids: body.ids,
    storage: body.storage.kind === "zfs"
      ? { kind: "zfs", useSpareDisk: body.storage.use_spare_disk, fallbackToDocker: body.storage.fallback_to_docker }
      : { kind: "none" },
    names: body.names,
    runs: body.runs,
  };
  return body.first === undefined ? input : { ...input, first: body.first };
}

function cliWaitingServer(server: WaitingServerView) {
  return {
    id: server.id,
    name: server.report.hostname,
    status: server.status,
    observed_ip: server.observedIp,
    fleet_token: server.fleetToken,
    report: server.report,
    first_seen_at: server.firstSeenAt,
    last_check_in_at: server.lastCheckInAt,
  };
}

function cliEnrollment(enrollment: ServerEnrollmentView) {
  return {
    id: enrollment.id,
    waiting_server_id: enrollment.waitingServerId,
    role: enrollment.role,
    name: enrollment.name,
    storage: enrollment.storage,
    state: enrollment.state,
    stage: enrollment.stage,
    queued: enrollment.queued,
    ahead: enrollment.ahead,
    machine_id: enrollment.machineId,
    failure: enrollment.failure,
    requested_at: enrollment.requestedAt,
    ended_at: enrollment.endedAt,
  };
}

/** Waiting Servers and the Server Enrollments that bring them in. */
export const enrollCliRoutes = {
  "GET waiting-servers": ({ caller }) =>
    Effect.gen(function* () {
      return {
        waiting_servers: (yield* listWaitingServers(caller, { organizationSlug: caller.organization.slug })).map(cliWaitingServer),
      };
    }),
  "GET waiting-servers/:id": ({ caller, id }) =>
    Effect.gen(function* () {
      if (!Schema.is(Uuid)(id)) return yield* new NotFound({ message: "No such waiting server." });
      return { waiting_server: cliWaitingServer(yield* showWaitingServer(caller, { organizationSlug: caller.organization.slug, id })) };
    }),
  "POST reject-servers": ({ caller, request }) =>
    Effect.gen(function* () {
      const input = yield* decodeBody(WaitingServerIds, request, "Rejecting takes the ids of 1 to 1000 waiting servers.");
      return yield* rejectWaitingServers(caller, { organizationSlug: caller.organization.slug, ids: input.ids });
    }),
  "POST probe-servers": ({ caller, request }) =>
    Effect.gen(function* () {
      const input = yield* decodeBody(WaitingServerIds, request, "Probing takes the ids of 1 to 1000 waiting servers.");
      return yield* openProbeSelection(caller, { organizationSlug: caller.organization.slug, ids: input.ids });
    }),
  "POST enrollments": ({ caller, request }) =>
    Effect.gen(function* () {
      const body = yield* decodeBody(EnrollServers, request, ENROLL_REFUSAL);
      const started = yield* startServerEnrollment(caller, { organizationSlug: caller.organization.slug, ...enrollInput(body) });
      return {
        enrollments: started.attempts.map((attempt) => ({
          id: attempt.id,
          waiting_server_id: attempt.waitingServerId,
          role: attempt.role,
          name: attempt.name,
          state: attempt.state,
        })),
        skipped: started.skipped,
        spread: started.spread,
      };
    }),
  "POST enrollments/:id": ({ caller, request, id }) =>
    Effect.gen(function* () {
      if (id !== "plan") return yield* new NotFound({ message: "Not found." });
      const body = yield* decodeBody(EnrollServers, request, ENROLL_REFUSAL);
      const plan = yield* previewServerEnrollment(caller, { organizationSlug: caller.organization.slug, ...enrollInput(body) });
      return {
        plan: plan.rows.map((row) => ({
          waiting_server_id: row.waitingServerId,
          hostname: row.hostname,
          role: row.role,
          name: row.instruction.name,
          storage: row.instruction.storage,
        })),
        skipped: plan.skipped,
        spread: plan.spread,
      };
    }),
  "GET enrollments": ({ caller }) =>
    Effect.gen(function* () {
      return { enrollments: (yield* readServerEnrollments(caller, { organizationSlug: caller.organization.slug })).map(cliEnrollment) };
    }),
  "GET enrollments/:id": ({ caller, id }) =>
    Effect.gen(function* () {
      if (!Schema.is(Uuid)(id)) return yield* new NotFound({ message: "No such enrollment." });
      const [found] = yield* readServerEnrollments(caller, { organizationSlug: caller.organization.slug, id });
      if (found === undefined) return yield* new NotFound({ message: "No such enrollment." });
      return { enrollment: cliEnrollment(found) };
    }),
  "POST enrollments/:id/cancel": ({ caller, id }) =>
    Effect.gen(function* () {
      if (!Schema.is(Uuid)(id)) return yield* new NotFound({ message: "No such enrollment." });
      return yield* cancelServerEnrollment(caller, { organizationSlug: caller.organization.slug, id });
    }),
} satisfies CliRoutes;
