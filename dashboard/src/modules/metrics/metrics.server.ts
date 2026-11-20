import "@tanstack/react-start/server-only";
import { Clock, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { findService } from "#/modules/config-store/store-lookup.server";
import { observeConfigured } from "#/modules/observe-store/clickhouse.server";
import { requireAccess } from "#/modules/organization/access.server";
import type { AppConfig } from "#/server/config.server";
import type { Unavailable } from "#/server/public-error";
import { resolveRange } from "#/modules/uptime/uptime-range";
import { hostHistory, markersOnlyHistory, serviceHistory, type CrashEvent, type DiskRow, type MetricRow } from "./metrics-history";
import { stepFor } from "./metrics-series";
import { machineIsReporting, readCrashEvents, readDiskRows, readHostRows, readServiceRows, type StoreRange } from "./metrics-store.server";
import type { HostHistory, MetricRead, MetricWindow, ReadRuntimeServiceMetricsInput, ReadServerMetricsInput, ReadServiceMetricsInput, ServiceHistory } from "./metrics";

/** Runs `reading` against this Cloud's ClickHouse, or answers that metrics aren't enabled. */
const readStore = Effect.fn("Metrics.readStore")(function* <History>(
  window: MetricWindow, at: number, reading: (range: StoreRange) => Effect.Effect<MetricRead<History>, Unavailable, AppConfig>,
) {
  if (!(yield* observeConfigured)) return { type: "not_configured", window, at } satisfies MetricRead<History>;
  return yield* reading({ window, stepMs: stepFor(window) });
});

/** The Service's rows and crash events as one read. Every Machine's rows sit in its own entry. */
const readService = Effect.fn("Metrics.readService")(function* (organizationId: string, namespace: string, service: string, window: MetricWindow, at: number, markersOnly: boolean) {
  const asRead = (range: StoreRange, rows: readonly MetricRow[], crashes: readonly CrashEvent[]): MetricRead<ServiceHistory> => ({
    type: "history", window, at, history: markersOnly ? markersOnlyHistory(crashes, { ...range, at }) : serviceHistory(rows, crashes, { ...range, at }),
  });
  // The verify dashboard has no ClickHouse; production builds drop this branch and the fixture with it.
  if (import.meta.env.DEV && process.env["PLOYZ_VERIFY_METRICS"] === "1") {
    const { verifyServiceData } = yield* Effect.promise(() => import("./metrics.verify-fixture"));
    const range = { window, stepMs: stepFor(window) };
    const { rows, crashes } = verifyServiceData(service, range);
    return asRead(range, rows, crashes);
  }
  return yield* readStore(window, at, (range) => Effect.gen(function* () {
    const crashes = yield* readCrashEvents(organizationId, range, { namespace, service });
    return asRead(range, markersOnly ? [] : yield* readServiceRows(organizationId, range, namespace, service), crashes);
  }));
});

/** A Service's metrics summed over every Server it runs on, with each Server's own. */
export const readServiceMetrics = Effect.fn("Metrics.readServiceMetrics")(function* (actor: Actor, input: ReadServiceMetricsInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "metrics:read");
  const at = yield* Clock.currentTimeMillis;
  const window = resolveRange(input, at);
  const found = yield* findService(organization.id, input.environmentId, input.service);
  // An undeployed Service has nothing on any Server yet. Its own name is no stand-in, since after a rename it can be another Service's runtime name.
  if (found === null) return { type: "history", window, at, history: { stepMs: stepFor(window), points: [], servers: [] } } satisfies MetricRead<ServiceHistory>;
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
  // A Server with rows in view has a history, gaps included. One with none is silent only if it is silent now too.
  const asRead = (range: StoreRange, rows: readonly MetricRow[], disk: readonly DiskRow[], reporting: boolean): MetricRead<HostHistory> =>
    reporting ? { type: "history", window, at, history: hostHistory(rows, disk, { ...range, at }) } : { type: "not_reporting", window, at };
  if (import.meta.env.DEV && process.env["PLOYZ_VERIFY_METRICS"] === "1") {
    const { verifyServerData } = yield* Effect.promise(() => import("./metrics.verify-fixture"));
    const range = { window, stepMs: stepFor(window) };
    const { rows, disk, reporting } = verifyServerData(input.machineId, range);
    return asRead(range, rows, disk, reporting);
  }
  return yield* readStore(window, at, (range) => Effect.gen(function* () {
    const [rows, disk] = yield* Effect.all([readHostRows(organization.id, range, input.machineId), readDiskRows(organization.id, range, input.machineId)]);
    const reporting = rows.length > 0 || disk.length > 0 || (yield* machineIsReporting(organization.id, input.machineId));
    return asRead(range, rows, disk, reporting);
  }));
});
