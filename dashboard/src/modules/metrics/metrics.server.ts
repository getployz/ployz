import "@tanstack/react-start/server-only";
import { Clock, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { findService } from "#/modules/config-store/store-lookup.server";
import { observeConfigured } from "#/modules/observe-store/clickhouse.server";
import { requireAccess } from "#/modules/organization/access.server";
import { AppConfig } from "#/server/config.server";
import { resolveRange } from "#/lib/time-range";
import { hostHistory, markersOnlyHistory, serviceHistory, type CrashEvent, type DiskRow, type MetricRow } from "./metrics-history";
import { silentSince, stepFor } from "./metrics-series";
import { readCrashEvents, readHostReadings, readServiceReadings, type StoreRange } from "./metrics-store.server";
import type { HostHistory, MetricRead, MetricWindow, ReadRuntimeServiceMetricsInput, ReadServerMetricsInput, ReadServiceMetricsInput, ServiceHistory } from "./metrics";

/** Runs `reading` against this Cloud's ClickHouse, or answers that metrics aren't enabled. */
const readStore = Effect.fn("Metrics.readStore")(function* <History, E, R>(
  window: MetricWindow, at: number, reading: (range: StoreRange) => Effect.Effect<MetricRead<History>, E, R>,
) {
  if (!(yield* observeConfigured)) return { type: "not_configured", window, at } satisfies MetricRead<History>;
  return yield* reading({ window, stepMs: stepFor(window) });
});

/** The Service's rows and crash events as one read. Every Machine's rows sit in its own entry. */
const readService = Effect.fn("Metrics.readService")(function* (organizationId: string, namespace: string, service: string, window: MetricWindow, at: number, markersOnly: boolean) {
  const asRead = (range: StoreRange, rows: readonly MetricRow[], crashes: readonly CrashEvent[], latest: number | null): MetricRead<ServiceHistory> => {
    const silent = latest === null ? undefined : silentSince(latest, window, at);
    return {
      type: "history", window, at, history: markersOnly ? markersOnlyHistory(crashes, { ...range, at }) : serviceHistory(rows, crashes, { ...range, at }),
      ...(silent !== undefined && { silentSince: silent }),
    };
  };
  // The verify dashboard has no ClickHouse; production builds drop this branch and the fixture with it.
  if (import.meta.env.DEV && (yield* AppConfig).verify.metrics) {
    const { verifyServiceData } = yield* Effect.promise(() => import("./metrics.verify-fixture"));
    const range = { window, stepMs: stepFor(window) };
    const { rows, crashes } = verifyServiceData(service, range);
    return asRead(range, rows, crashes, rows.length > 0 ? at : null);
  }
  return yield* readStore(window, at, (range) => Effect.gen(function* () {
    const crashes = yield* readCrashEvents(organizationId, range, { namespace, service });
    if (markersOnly) return asRead(range, [], crashes, null);
    const { rows, latest } = yield* readServiceReadings(organizationId, range, namespace, service);
    return asRead(range, rows, crashes, latest);
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
  // A Server with rows in view has a history, gaps included, which says when it went silent. One with none is not
  // reporting only if it is silent now too.
  const asRead = (range: StoreRange, rows: readonly MetricRow[], disk: readonly DiskRow[], latest: number | null): MetricRead<HostHistory> => {
    const silent = latest === null ? undefined : silentSince(latest, window, at);
    if (rows.length === 0 && disk.length === 0 && (latest === null || silent !== undefined)) return { type: "not_reporting", window, at };
    return { type: "history", window, at, history: hostHistory(rows, disk, { ...range, at }), ...(silent !== undefined && { silentSince: silent }) };
  };
  if (import.meta.env.DEV && (yield* AppConfig).verify.metrics) {
    const { verifyServerData } = yield* Effect.promise(() => import("./metrics.verify-fixture"));
    const range = { window, stepMs: stepFor(window) };
    const { rows, disk, reporting } = verifyServerData(input.machineId, range);
    return asRead(range, rows, disk, reporting ? at : null);
  }
  return yield* readStore(window, at, (range) => Effect.gen(function* () {
    const { rows, disk, latest } = yield* readHostReadings(organization.id, range, input.machineId, at);
    return asRead(range, rows, disk, latest);
  }));
});
