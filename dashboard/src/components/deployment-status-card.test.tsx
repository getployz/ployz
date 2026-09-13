import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import type { EnvironmentDeploymentSummary } from "#/modules/deployments/deployment-contract";
import type { DeploymentProgressRow } from "#/modules/deployments/deployment-progress";
import { DeploymentStatusCard } from "./deployment-status-card";

const row = (serviceId: string, status: DeploymentProgressRow["status"]): DeploymentProgressRow => ({
  index: serviceId === "postgres" ? 0 : 1, serviceId, runtimeServiceId: serviceId, serviceName: serviceId,
  machineId: "machine", machineName: "server", displayName: null, target: null, updateOrder: "start_first",
  operation: "replace_container", status, phase: null, elapsedMs: null, deadlineMs: null, health: null,
  error: status === "failed" ? "Health check timed out" : null,
});
it("shows an applied service independently of a later failure, and preserves the environment partial result", () => {
  const deployment = asTestDouble<EnvironmentDeploymentSummary>()({ status: "failed", createdAt: new Date(), serviceCount: 2, volumeRemoveAttempts: [], failureCode: "sdk_deploy_failed" });
  const progress = { completed: 1, total: 2, outcome: "failed" as const, rows: [row("postgres", "completed"), row("web", "failed")], compensation: [] };
  const render = (serviceId?: string) => renderToStaticMarkup(createElement(DeploymentStatusCard, {
    deployment, progress, serviceId, expanded: true, onExpandedChange() {}, showLogs: false, onLogsChange() {}, actions: null, logsPanel: null,
  }));
  const service = render("postgres");
  expect(service).toContain("Service applied");
  expect(service).toContain("border-success/30");
  expect(service).not.toContain("Health check timed out");
  expect(render()).toContain("Rollout partially applied");
  expect(render("web")).toContain("Health check timed out");
});
it("does not claim an unknown runtime outcome was never attempted", () => {
  const deployment = asTestDouble<EnvironmentDeploymentSummary>()({ status: "failed", createdAt: new Date(), failureCode: "sdk_deploy_outcome_unknown", serviceCount: 1 });
  const html = renderToStaticMarkup(createElement(DeploymentStatusCard, { deployment, progress: null, expanded: true, onExpandedChange() {}, showLogs: false, onLogsChange() {}, actions: null, logsPanel: null }));
  expect(html).toContain("runtime outcome unknown");
  expect(html).toContain("Runtime outcome unavailable");
});
