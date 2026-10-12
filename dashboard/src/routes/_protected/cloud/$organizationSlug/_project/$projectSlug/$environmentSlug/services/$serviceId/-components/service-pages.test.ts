import { expect, it } from "vitest";
import { monitoringSections } from "./monitoring/monitoring-sections";
import { servicePages } from "./service-pages";

const hasMonitoring = (uptimeEnabled: boolean, metrics: boolean) =>
  servicePages(monitoringSections({ uptimeEnabled, metrics })).some((page) => page.id === "monitoring");

it("drops the Monitoring tab only when it has no section left", () => {
  expect([hasMonitoring(true, false), hasMonitoring(true, true), hasMonitoring(false, true), hasMonitoring(false, false)])
    .toEqual([true, true, true, false]);
});

it("keeps every other tab", () => {
  expect(servicePages([]).map((page) => page.id)).toEqual(["deployments", "variables", "logs", "settings"]);
});
