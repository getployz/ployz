import "@tanstack/react-start/server-only";
import type { MetricHistoryInput } from "@ployz/sdk";
import { Clock, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { findService } from "#/modules/config-store/store-lookup.server";
import { openObserveStore, type OrgQuery } from "#/modules/observe-store/clickhouse.server";
import { requireAccess } from "#/modules/organization/access.server";
import type { Unavailable } from "#/server/public-error";
import { resolveRange } from "#/modules/uptime/uptime-range";
import { hostHistory, markersOnlyHistory, serviceHistory } from "./metrics-history";
import { stepFor } from "./metrics-series";
import { machineIsReporting, readCrashEvents, readDiskRows, readHostRows, readServiceRows, type StoreRange } from "./metrics-store.server";
import type { MetricRead, MetricWindow, ReadRuntimeServiceMetricsInput, ReadServerMetricsInput, ReadServiceMetricsInput } from "./metrics";

const nanos = (ms: number) => (BigInt(ms) * 1_000_000n).toString();

type Reading = (query: OrgQuery, range: StoreRange) => Effect.Effect<MetricRead, Unavailable>;

/** Opens the Organization's ClickHouse and runs `reading` against it, or answers that metrics aren't enabled. */
const readStore = Effect.fn("Metrics.readStore")(function* (organizationId: string, window: MetricWindow, at: number, reading: Reading) {
  return yield* Effect.scoped(Effect.gen(function* () {
    const store = yield* openObserveStore(organizationId);
    if (store.type === "not_configured") return { type: "not_configured", window, at } satisfies MetricRead;
    return yield* reading(store.query, { window, stepMs: stepFor(window) });
  }));
});

/** The Service's rows and crash events as one read. Every Machine's rows sit in its own reply. */
const readService = Effect.fn("Metrics.readService")(function* (organizationId: string, namespace: string, service: string, window: MetricWindow, at: number, markersOnly: boolean) {
  // The verify dashboard has no Servers to ask; production builds drop this branch and the fixture with it.
  if (import.meta.env.DEV && process.env["PLOYZ_VERIFY_METRICS"] === "1") {
    const input: MetricHistoryInput = { subject: { kind: "service", service: `${namespace}/${service}` }, since_nanos: nanos(window.from), until_nanos: nanos(window.to) };
    const { verifyMetricHistory } = yield* Effect.promise(() => import("./metrics.verify-fixture"));
    return { type: "history", window, at, history: verifyMetricHistory(input) } satisfies MetricRead;
  }
  return yield* readStore(organizationId, window, at, (query, range) => Effect.gen(function* () {
    const crashes = yield* readCrashEvents(query, range, { namespace, service });
    if (markersOnly) return { type: "history", window, at, history: markersOnlyHistory(crashes, { ...range, at }) } satisfies MetricRead;
    const rows = yield* readServiceRows(query, range, namespace, service);
    return { type: "history", window, at, history: serviceHistory(rows, crashes, { ...range, at }) } satisfies MetricRead;
  }));
});

/** A Service's metrics summed over every Server it runs on, with each Server's own reply. */
export const readServiceMetrics = Effect.fn("Metrics.readServiceMetrics")(function* (actor: Actor, input: ReadServiceMetricsInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "metrics:read");
  const at = yield* Clock.currentTimeMillis;
  const window = resolveRange(input, at);
  const found = yield* findService(organization.id, input.environmentId, input.service);
  // An undeployed Service has nothing on any Server yet. Its own name is no stand-in, since after a rename it can be another Service's runtime name.
  if (found === null) return { type: "history", window, at, history: { points: { service: [] }, machines: [], unanswered: [] } } satisfies MetricRead;
  return yield* readService(organization.id, found.namespace, found.runtime, window, at, input.markersOnly === true);
});

export const readRuntimeServiceMetrics = Effect.fn("Metrics.readRuntimeService")(function* (actor: Actor, input: ReadRuntimeServiceMetricsInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "metrics:read");
  const at = yield* Clock.currentTimeMillis;
  return yield* readService(organization.id, input.namespace, input.name, resolveRange(input, at), at, false);
});

export const readServerMetrics = Effect.fn("Metrics.readServer")(function* (actor: Actor, input: ReadServerMetricsInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "metrics:read");
  const at = yield* Clock.currentTimeMillis;
  const window = resolveRange(input, at);
  if (import.meta.env.DEV && process.env["PLOYZ_VERIFY_METRICS"] === "1") {
    const request: MetricHistoryInput = { subject: { kind: "machine", machine_id: input.machineId }, since_nanos: nanos(window.from), until_nanos: nanos(window.to) };
    const { verifyMetricHistory } = yield* Effect.promise(() => import("./metrics.verify-fixture"));
    return { type: "history", window, at, history: verifyMetricHistory(request) } satisfies MetricRead;
  }
  return yield* readStore(organization.id, window, at, (query, range) => Effect.gen(function* () {
    const [rows, disk] = yield* Effect.all([readHostRows(query, range, input.machineId), readDiskRows(query, range, input.machineId)]);
    // A Server with rows in view has a history, gaps included. One with none is silent only if it is silent now too.
    if (rows.length === 0 && disk.length === 0 && !(yield* machineIsReporting(query, input.machineId))) {
      return { type: "not_reporting", window, at } satisfies MetricRead;
    }
    return { type: "history", window, at, history: hostHistory(input.machineId, rows, disk, { ...range, at }) } satisfies MetricRead;
  }));
});
