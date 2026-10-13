import "@tanstack/react-start/server-only";
import type { MachineId, MetricHistoryInput, MetricSubject } from "@ployz/sdk";
import { Clock, Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { findService } from "#/modules/config-store/store-lookup.server";
import { requireAccess } from "#/modules/organization/access.server";
import { OrganizationRuntime } from "#/modules/runtime/organization-runtime.server";
import { resolveRange } from "#/modules/uptime/uptime-range";
import type { MetricRead, MetricWindow, ReadServerMetricsInput, ReadServiceMetricsInput } from "./metrics";

const nanos = (ms: number) => (BigInt(ms) * 1_000_000n).toString();

const readHistory = Effect.fn("Metrics.readHistory")(function* (organizationId: string, subject: MetricSubject, window: MetricWindow) {
  const input: MetricHistoryInput = { subject, since_nanos: nanos(window.from), until_nanos: nanos(window.to) };
  // The verify dashboard has no Servers to ask; production builds drop this branch and the fixture with it.
  if (import.meta.env.DEV && process.env["PLOYZ_VERIFY_METRICS"] === "1") {
    const { verifyMetricHistory } = yield* Effect.promise(() => import("./metrics.verify-fixture"));
    return { type: "history", window, history: verifyMetricHistory(input) } satisfies MetricRead;
  }
  return yield* Effect.scoped(Effect.gen(function* () {
    const session = yield* (yield* OrganizationRuntime).open(organizationId);
    if (session.status !== "connected") return { type: "offline", window } satisfies MetricRead;
    const history = yield* session.connected.metricHistory(input);
    return { type: "history", window, history } satisfies MetricRead;
  }));
});

/** A Service's metrics summed over every Server it runs on, with each Server's own reply. */
export const readServiceMetrics = Effect.fn("Metrics.readService")(function* (actor: Actor, input: ReadServiceMetricsInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const window = resolveRange(input, yield* Clock.currentTimeMillis);
  const found = yield* findService(organization.id, input.environmentId, input.service);
  // An undeployed Service has nothing on any Server yet. Its own name is no stand-in, since after a rename it can be another Service's runtime name.
  if (found === null) return { type: "history", window, history: { points: { service: [] }, machines: [], unanswered: [] } } satisfies MetricRead;
  return yield* readHistory(organization.id, { kind: "service", service: `${found.namespace}/${found.runtime}` }, window);
});

export const readServerMetrics = Effect.fn("Metrics.readServer")(function* (actor: Actor, input: ReadServerMetricsInput) {
  const { organization } = yield* requireAccess(actor, input.organizationSlug, "operate");
  const window = resolveRange(input, yield* Clock.currentTimeMillis);
  // SAFETY: the input schema checks the 32 lowercase hex digits a MachineId is.
  const machineId = input.machineId as MachineId;
  return yield* readHistory(organization.id, { kind: "machine", machine_id: machineId }, window);
});
