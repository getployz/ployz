// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { parseServiceConfig } from "@ployz/sdk/config";
import { buildCanvasEnvironmentChangeState } from "#/modules/environment-design/canvas-environment-change-state";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import { ApplyChangesBar } from "./ApplyChangesBar";

afterEach(cleanup);
const state = (replicas: number) => ({
  token: String(replicas), nodes: [{ node: { type: "service" as const, id: "api" }, config: parseServiceConfig({
    version: 2, name: "API", privateDns: "api", replicas,
    source: { version: 1, type: "empty", rootDir: "/" },
    healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
  }) }],
});
const props = {
  commitMessage: "", onCommitMessageChange: vi.fn(), onDeploy: vi.fn(),
  onSaveWithoutDeploying: vi.fn(), onDiscardAll: vi.fn(), onDiscardNode: vi.fn(), onDiscardRow: vi.fn(),
};
function bar(status: EnvironmentDeploymentStatus, working: number) {
  const changes = buildCanvasEnvironmentChangeState({
    working: state(working), saved: state(5), applied: state(status === "applied" ? 5 : 1),
    deploymentEvidence: { id: "attempt", status, ...state(5) },
    nodeIntroductions: { token: "none", nodes: [] },
    nodes: [{ node: { type: "service", id: "api" }, name: "API", summaryLabel: "API" }],
  });
  return <ApplyChangesBar {...props} groups={changes.groups} totalChanges={changes.totalCount} canSaveWithoutDeploying={changes.canSave} />;
}

it("hides accepted changes, then shows only the new edit", () => {
  const view = render(bar("deploying", 5));
  expect(screen.queryByText(/Apply .*change/)).toBeNull();
  view.rerender(bar("deploying", 7));
  expect(screen.getByText("Apply 1 change")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Details" }));
  expect(screen.getAllByText("API")).toHaveLength(1);
  expect(screen.getByText("5")).toBeTruthy();
  expect(screen.getByText("7")).toBeTruthy();
  expect(screen.queryByText(/Unsaved|Pending|Drift/)).toBeNull();
});

it("automatically exposes the outstanding diff after failure", () => {
  const view = render(bar("deploying", 5));
  view.rerender(bar("failed", 5));
  expect(screen.getByText("Apply 1 change")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Details" }));
  expect(screen.getByText("1")).toBeTruthy();
  expect(screen.getByText("5")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Deploy changes" }));
  expect(props.onDeploy).toHaveBeenCalled();
});
