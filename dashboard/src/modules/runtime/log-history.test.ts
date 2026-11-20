import { expect, it } from "vitest";
import { type LogHistoryStatus, logOffStates, NOT_REPORTING_MS } from "./log-history";
import type { RuntimeContainerRecord } from "./runtime.collection";

const NOW = 1_800_000_000_000;
const container = (machineId: string, logDriver: string | undefined, state = "running"): RuntimeContainerRecord => {
  const record: RuntimeContainerRecord = { id: `${machineId}-c`, displayName: "web", machineId, namespace: "shop-prod", kind: "service", runtime: { state } };
  return logDriver === undefined ? record : { ...record, logDriver };
};
const status = (seen: Record<string, number>, history = true): LogHistoryStatus => ({
  history, now: NOW, machines: Object.entries(seen).map(([machineId, ago]) => ({ machineId, seenAt: NOW - ago })),
});

it("calls a Machine not reporting once it has sent nothing for five minutes, and not before", () => {
  const containers = [container("fresh", "json-file"), container("stale", "json-file"), container("never", "json-file")];
  const { silent } = logOffStates({ status: status({ fresh: NOT_REPORTING_MS, stale: NOT_REPORTING_MS + 1 }), containers, now: NOW });
  expect(silent).toEqual([{ machineId: "stale", silentMs: NOT_REPORTING_MS + 1 }, { machineId: "never", silentMs: null }]);
});

it("says a Service on the local driver is not sending yet, without also calling its Machine quiet", () => {
  const containers = [container("old", "local"), container("unknown", undefined)];
  expect(logOffStates({ status: status({}), containers, now: NOW })).toEqual({ onLocal: true, silent: [] });
});

it("says nothing about a stopped container, and nothing until the status is known", () => {
  expect(logOffStates({ status: status({}), containers: [container("gone", "json-file", "exited")], now: NOW })).toEqual({ onLocal: false, silent: [] });
  expect(logOffStates({ status: undefined, containers: [container("old", "local")], now: NOW })).toEqual({ onLocal: false, silent: [] });
});

it("has no off state of its own when the Cloud keeps no history", () => {
  expect(logOffStates({ status: status({}, false), containers: [container("old", "local"), container("new", "json-file")], now: NOW })).toEqual({ onLocal: false, silent: [] });
});
