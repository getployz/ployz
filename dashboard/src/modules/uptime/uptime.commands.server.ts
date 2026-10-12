import "@tanstack/react-start/server-only";
import { Effect, Schedule, Schema } from "effect";
import { defineCommand, refused } from "#/modules/commands/command.server";
import { readStore } from "#/modules/config-store/config-store.server";
import type { Caller } from "#/modules/identity/actor";
import { causeText, observationText, uptimePercent } from "./uptime";
import { DEFAULT_RANGE, isRangeEdge } from "./uptime-range";
import { type RegionNames, type ServiceUptime, type UptimeCheckResult, IncidentObservation } from "./uptime-status";
import { readServiceUptime, requestUptimeProbe } from "./uptime-status.server";

const Name = (description: string) => Schema.NonEmptyString.annotate({ description });

const ServiceScope = {
  service: Name("The Service, by name"),
  project: Schema.optionalKey(Name("Its Project, by name. The default Project when left out")),
  environment: Schema.optionalKey(Name("Its Environment, by name. The Project's default Environment when left out")),
};
type ServiceScope = { readonly service: string; readonly project?: string; readonly environment?: string };

const Edge = (description: string) => Schema.optionalKey(Schema.String.annotate({ description }).check(isRangeEdge));

const Result = Schema.Struct({
  region: Schema.String,
  at: Schema.String,
  kind: IncidentObservation.fields.kind,
  verdict: IncidentObservation.fields.verdict,
  answer: Schema.String,
  ms: Schema.Number,
});

const Region = Schema.Struct({
  region: Schema.String,
  latest: Schema.NullOr(Result),
  last_error: Schema.NullOr(Schema.Struct({ at: Schema.String, error: Schema.String })),
});

const Incident = Schema.Struct({
  opened_at: Schema.String,
  closed_at: Schema.NullOr(Schema.String),
  cause: Schema.String,
  observations: Schema.Array(Schema.Struct({
    region: Schema.String,
    at: Schema.String,
    verdict: IncidentObservation.fields.verdict,
    answer: Schema.String,
  })),
});

const Status = Schema.Literals(["pending", "up", "unreachable"]);

const Shown = Schema.Struct({
  service: Schema.String,
  url: Schema.String,
  status: Status,
  checking: Schema.Boolean,
  uptime: Schema.NullOr(Schema.String),
  regions: Schema.Array(Region),
  incidents: Schema.Array(Incident),
});

const Checked = Schema.Struct({
  service: Schema.String,
  url: Schema.String,
  status: Status,
  answered: Schema.Boolean,
  results: Schema.Array(Result),
});

type Found = { readonly name: string; readonly environmentId: string; readonly serviceId: string };

const found = (caller: Caller, scope: ServiceScope) =>
  readStore(caller.organization.id, {
    query: "service",
    environment: { project: scope.project ?? null, environment: scope.environment ?? null },
    service: scope.service,
  }).pipe(
    Effect.map((view): Found => ({ name: view.name, environmentId: view.environment.id, serviceId: view.id })),
  );

const regionName = (regions: RegionNames, code: string) => regions[code] ?? code;

const resultLine = (regions: RegionNames, result: UptimeCheckResult): typeof Result.Type => ({
  region: regionName(regions, result.region),
  at: result.at,
  kind: result.kind,
  verdict: result.verdict,
  answer: result.status === 0 ? "No response" : String(result.status),
  ms: result.ms,
});

const shown = (name: string, uptime: ServiceUptime): typeof Shown.Type => ({
  service: name,
  url: uptime.check.url,
  status: uptime.status,
  checking: uptime.probe !== null && !uptime.probe.answered,
  uptime: uptimePercent(uptime.uptime),
  regions: Object.keys(uptime.regions).sort().map((code) => {
    const latest = (uptime.checks ?? []).filter((result) => result.region === code).at(-1);
    const error = uptime.points.filter((point) => point.region === code && point.lastError !== null).at(-1)?.lastError ?? null;
    return {
      region: regionName(uptime.regions, code),
      latest: latest === undefined ? null : resultLine(uptime.regions, latest),
      last_error: error === null ? null : { at: error.at, error: error.text },
    };
  }),
  incidents: uptime.incidents.map((incident) => ({
    opened_at: incident.openedAt,
    closed_at: incident.closedAt,
    cause: causeText(incident),
    observations: incident.observations.map((observation) => ({
      region: regionName(uptime.regions, observation.region),
      at: observation.at,
      verdict: observation.verdict,
      answer: observationText(observation),
    })),
  })),
});

const notChecked = (name: string) =>
  refused("not_checked", `${name} has no uptime check: it has no public address. Add a domain to check it.`);

export const uptimeShow = defineCommand({
  name: "uptime_show",
  path: ["uptime", "show"],
  about: "Show a Service's uptime: whether visitors reach it, what each region's check got back, and its incidents",
  examples: ["ployz uptime show web", "ployz uptime show web --project shop --environment staging --from 6h"],
  kind: "read",
  agents: true,
  typed: null,
  positional: ["service"],
  input: Schema.Struct({
    ...ServiceScope,
    from: Edge("Where the range starts: an offset back from now such as 15m, 6h or 7d, or a UTC instant. 1h when left out"),
    to: Edge("Where the range ends: now, an offset back from now, or a UTC instant. now when left out"),
  }),
  output: Shown,
  render: "record",
  run: (caller, input) => Effect.gen(function* () {
    const service = yield* found(caller, input);
    const uptime = yield* readServiceUptime(caller, {
      organizationSlug: caller.organization.slug,
      environmentId: service.environmentId,
      serviceId: service.serviceId,
      from: input.from ?? DEFAULT_RANGE.from,
      to: input.to ?? DEFAULT_RANGE.to,
    });
    return uptime === null ? notChecked(service.name) : { ok: true, output: shown(service.name, uptime) } as const;
  }),
});

/**
 * Asks for a probe the way Check now does, then waits while the read still shows it in flight: until the checker judges
 * the round, or until the read stops showing the request because the slowest round is over.
 */
export const uptimeCheck = defineCommand({
  name: "uptime_check",
  path: ["uptime", "check"],
  about: "Check a Service's public address now, from every region, and show what each got back. Changes no setting",
  examples: ["ployz uptime check web", "ployz uptime check web --project shop --environment staging"],
  kind: "read",
  agents: true,
  typed: null,
  positional: ["service"],
  input: Schema.Struct(ServiceScope),
  output: Checked,
  render: "record",
  run: (caller, input) => Effect.gen(function* () {
    const service = yield* found(caller, input);
    const scope = { organizationSlug: caller.organization.slug, environmentId: service.environmentId, serviceId: service.serviceId };
    yield* requestUptimeProbe(caller, scope);
    const read = readServiceUptime(caller, { ...scope, ...DEFAULT_RANGE });
    const uptime = yield* read.pipe(Effect.repeat({
      schedule: Schedule.spaced("1 second"),
      while: (current) => current?.probe?.answered === false,
    }));
    if (uptime === null) return notChecked(service.name);
    const answered = uptime.probe?.answered ?? false;
    const requestedAt = uptime.probe?.requestedAt ?? null;
    const round = requestedAt === null ? [] : (uptime.checks ?? []).filter((result) => result.kind === "triggered" && result.at >= requestedAt);
    const output = {
      service: service.name,
      url: uptime.check.url,
      status: uptime.status,
      answered,
      results: round.map((result) => resultLine(uptime.regions, result)),
    };
    if (answered) return { ok: true, output } as const;
    return { ok: true, output, message: `The check hasn't answered yet. Run ployz uptime show ${service.name} to see it land.` } as const;
  }),
});
