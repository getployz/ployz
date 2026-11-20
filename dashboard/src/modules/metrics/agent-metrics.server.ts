import "@tanstack/react-start/server-only";
import type { MachineId, MetricEvent, MetricsAnswer, MetricsQuery } from "@ployz/sdk";
import { Effect } from "effect";
import { observeConfigured } from "#/modules/observe-store/clickhouse.server";
import { serverNames } from "#/modules/operations/operation-history.server";
import { hostHistory, serviceHistory, type CrashEvent } from "./metrics-history";
import { serverMetrics, serviceMetrics } from "./metrics-model";
import { stepFor } from "./metrics-series";
import { gapNotes, serverRows, serviceRows } from "./metrics-summary";
import { countCrashEvents, machineIsReporting, readCrashEvents, readDiskRows, readHostRows, readServiceRows } from "./metrics-store.server";

/** What the agent hears: the answer, or that Cloud keeps no metrics. */
export type AgentMetrics = { readonly type: "not_configured" } | { readonly type: "answered"; readonly answer: MetricsAnswer };

const EVENT_LIMIT = 50;

const eventOf = (crash: CrashEvent, names: ReadonlyMap<string, string>): MetricEvent => ({
  at: Math.floor(crash.at / 1000),
  server: names.get(crash.machine) ?? crash.machine,
  namespace: crash.namespace,
  service: crash.service,
  deployment: null,
  ...(crash.reason === "out_of_memory"
    ? { kind: "oom" as const }
    : { kind: "exit" as const, ...(crash.exitCode !== null && { exit_code: crash.exitCode }), oom: false, stopped: false }),
});

/**
 * Answers the agent's `metrics` question from Cloud's ClickHouse, in Cloud's name and the Organization's rows only.
 * `readable` bounds the Namespaces of the crash events it lists; the caller has already checked the permission.
 */
export const answerMetrics = Effect.fn("Metrics.answerForAgent")(function* (
  organizationId: string, query: MetricsQuery, now: number, readable: ReadonlyArray<string> | null,
) {
  const until = Math.floor(now / 1000);
  const since = query.since.kind === "at" ? query.since.at : until - query.since.minutes * 60;
  const window = { from: since * 1000, to: until * 1000 };
  if (!(yield* observeConfigured)) return { type: "not_configured" } satisfies AgentMetrics;
  const range = { window, stepMs: stepFor(window) };
  const reading = { ...range, at: now };
  const { subject } = query;
  // A token held to Environments hears of crashes in the Namespaces of those Environments only; `null` is every Namespace.
  const crashFilter = {
    ...(subject.kind === "service" ? { namespace: subject.namespace, service: subject.service } : { machine: subject.server.machine }),
    ...(readable !== null && { namespaces: readable }),
  };
  const [crashes, counted] = yield* Effect.all([readCrashEvents(organizationId, range, crashFilter), countCrashEvents(organizationId, range, crashFilter)]);
  const notes: string[] = [];
  let metrics: MetricsAnswer["metrics"] = [];
  if (subject.kind === "service") {
    const rows = yield* readServiceRows(organizationId, range, subject.namespace, subject.service);
    const history = serviceHistory(rows, crashes, reading);
    const charts = serviceMetrics({ type: "history", window, at: now, history }, new Map());
    metrics = serviceRows(charts, window.to);
    notes.push(...gapNotes(charts.allGaps));
  } else {
    const machine = subject.server.machine;
    const [rows, disk] = yield* Effect.all([readHostRows(organizationId, range, machine), readDiskRows(organizationId, range, machine)]);
    if (rows.length === 0 && disk.length === 0 && !(yield* machineIsReporting(organizationId, machine))) {
      notes.push(`${subject.server.name} isn’t sending metrics.`);
    } else {
      const charts = serverMetrics({ type: "history", window, at: now, history: hostHistory(rows, disk, reading) });
      metrics = serverRows(charts, window.to);
      notes.push(...gapNotes(charts.gaps));
    }
  }
  if (metrics.length === 0 && notes.length === 0) notes.push("No readings in this range.");
  if (query.deployment !== null) notes.push("Metrics and crash events aren’t split by Deployment, so these cover every Deployment.");
  notes.push("Restarts aren’t recorded.");
  const total = counted.oomKills + counted.exits;
  const listed = Math.min(crashes.length, EVENT_LIMIT);
  if (total > listed) notes.push(`The events are the newest ${listed} of ${total} crashes; the counts cover all ${total}.`);
  // SAFETY: crash events carry the Machine ids Cloud's Collector stamped, which are the ids `serverNames` keys by.
  const machines = [...new Set(crashes.map((crash) => crash.machine))] as MachineId[];
  const names = yield* serverNames(organizationId, machines);
  const newest = [...crashes].sort((left, right) => right.at - left.at);
  const answer: MetricsAnswer = {
    since, until, metrics,
    oom_kills: counted.oomKills,
    exits: counted.exits,
    restarts: 0,
    events: newest.slice(0, listed).map((crash) => eventOf(crash, names)),
    notes,
    seen: { seen_by: null, observed_at: until, read_at: until, age_seconds: 0, from: "cloud_cache", unanswered: [] },
  };
  return { type: "answered", answer } satisfies AgentMetrics;
});
