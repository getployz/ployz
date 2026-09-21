// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { parseDashboardServiceConfig as parseServiceConfig } from "#/modules/environment-design/service-config";
import { buildCanvasEnvironmentChangeState } from "#/modules/environment-design/canvas-environment-change-state";
import type { EnvironmentDeploymentStatus } from "#/modules/deployments/tables";
import { ApplyChangesBar } from "./ApplyChangesBar";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});
const state = (replicas: number) => ({
  token: String(replicas), nodes: [{ node: { type: "service" as const, id: "api" }, config: parseServiceConfig({
    version: 2, privateDns: "api", replicas,
    source: { version: 1, type: "empty", rootDir: "/" },
    healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
  }) }],
});
const props = {
  commitMessage: "", onCommitMessageChange: vi.fn(), onDeploy: vi.fn(),
  onSaveWithoutDeploying: vi.fn(), onDiscardAll: vi.fn(), onDiscardNode: vi.fn(), onDiscardRow: vi.fn(),
};
function bar(status: EnvironmentDeploymentStatus | null, working: number, saved = 5) {
  const changes = buildCanvasEnvironmentChangeState({
    working: state(working), applied: state(status === "applied" ? 5 : 1),
    saved: state(saved),
    deploymentEvidence: status ? { id: "attempt", status, ...state(5) } : null,
    nodeIntroductions: { token: "none", nodes: [] },
    nodes: [{ node: { type: "service", id: "api" }, name: "API", summaryLabel: "API" }],
  });
  return <ApplyChangesBar {...props} groups={changes.groups} totalChanges={changes.totalCount} canSaveWithoutDeploying={changes.canSave} />;
}

it.each([null, "deploying"] as const)("can publish a revert to Head while a different revision is Saved (%s)", (status) => {
  const working = status === "deploying" ? 5 : 1;
  const view = render(bar(status, working, 7));
  expect(screen.queryByText(/Apply .*change/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
  expect(screen.queryByRole("button", { name: /^Deploy/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Save without deploying" }));
  expect(props.onSaveWithoutDeploying).toHaveBeenCalledTimes(1);

  view.rerender(bar(status, working, working));
  expect(screen.queryByRole("button", { name: "Save without deploying" })).toBeNull();
  expect(screen.queryByText("Unpublished changes")).toBeNull();
});

it("hides accepted changes, then shows only the new edit", () => {
  const view = render(bar("deploying", 5));
  expect(screen.queryByText(/Apply .*change/)).toBeNull();
  view.rerender(bar("deploying", 7));
  expect(screen.getByText("1 change")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
  expect(screen.getAllByText("API")).toHaveLength(1);
  expect(screen.getByText("5")).toBeTruthy();
  expect(screen.getByText("7")).toBeTruthy();
  expect(screen.getByRole("dialog", { name: "Environment changes" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  expect(screen.queryByRole("region", { name: "Environment changes" })).toBeNull();
  expect(document.activeElement).toBe(screen.getByRole("button", { name: "Review changes" }));
});

it("automatically exposes the outstanding diff after failure", () => {
  const view = render(bar("deploying", 5));
  view.rerender(bar("failed", 5));
  expect(screen.getByText("1 change")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Review changes" }));
  expect(screen.getByText("1")).toBeTruthy();
  expect(screen.getByText("5")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Deploy changes" }));
  expect(props.onDeploy).toHaveBeenCalled();
});
