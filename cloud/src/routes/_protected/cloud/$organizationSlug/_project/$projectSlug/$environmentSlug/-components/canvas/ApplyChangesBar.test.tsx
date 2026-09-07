// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CanvasEnvironmentChangeGroup } from "#/modules/environment-design/canvas-environment-change-state";
import { ApplyChangesBar } from "./ApplyChangesBar";

const group: CanvasEnvironmentChangeGroup = {
  nodeType: "service",
  nodeId: "00000000-0000-4000-8000-000000000001",
  nodeName: "API",
  summaryLabel: "API",
  lifecycle: "update",
  slice: "unsaved",
  changeCount: 1,
  projectedChange: {
    id: "service:00000000-0000-4000-8000-000000000001",
    node: {
      type: "service",
      id: "00000000-0000-4000-8000-000000000001",
    },
    presence: { baseline: "present", target: "present" },
    lifecycle: {
      id: "service:00000000-0000-4000-8000-000000000001:lifecycle",
      owner: {
        node: {
          type: "service",
          id: "00000000-0000-4000-8000-000000000001",
        },
      },
      kind: "update",
      resettable: true,
    },
    settings: [],
    discardPlan: null,
  },
  rows: [
    {
      changeKey: "service:name",
      label: "Name",
      kind: "update",
      path: "name",
      currentValue: "Old API",
      newValue: "API",
      canDiscard: true,
    },
  ],
};

const discardAllPlan = (enabled: boolean) => ({
  nodes: enabled
    ? [
        {
          node: group.projectedChange.node,
          working: {
            kind: "delete" as const,
            target: "working" as const,
            node: group.projectedChange.node,
          },
        },
      ]
    : [],
  savedCommand: null,
});

function bar(totalChanges: number) {
  return (
    <ApplyChangesBar
      groups={totalChanges > 0 ? [group] : []}
      totalChanges={totalChanges}
      discardAllPlan={discardAllPlan(totalChanges > 0)}
      commitMessage=""
      canSaveWithoutDeploying
      onCommitMessageChange={vi.fn()}
      onDeploy={vi.fn()}
      onSaveWithoutDeploying={vi.fn()}
      onDiscardAll={vi.fn()}
      onDiscardNode={vi.fn()}
      onDiscardRow={vi.fn()}
    />
  );
}

const slice = (kind: "unsaved" | "pending" | "drift", totalCount: number) => ({
  kind,
  provenance: {
    baseline: { role: kind === "unsaved" ? "saved" as const : "applied" as const, token: "baseline" },
    target: { role: kind === "unsaved" ? "working" as const : kind === "pending" ? "saved" as const : "runtime_observation" as const, token: "target" },
  },
  groups: [],
  lifecycleCount: 0,
  settingCount: totalCount,
  totalCount,
  discardPlans: { nodes: [], settings: [] },
});

afterEach(cleanup);

describe("ApplyChangesBar", () => {
  it("renders exactly while changes are present without a deployment label", () => {
    const view = render(bar(1));

    expect(screen.getByText("Apply 1 change")).toBeTruthy();
    expect(screen.queryByText(/Deploying/u)).toBeNull();

    view.rerender(bar(0));
    expect(screen.queryByText(/Apply 1 change/u)).toBeNull();
  });

  it("shows explicit slice counts and deployment evidence without a deploy affordance", () => {
    render(
      <ApplyChangesBar
        groups={[group]}
        slices={{
          unsaved: slice("unsaved", 1),
          pending: slice("pending", 2),
          drift: slice("drift", 3),
        }}
        totalChanges={6}
        discardAllPlan={discardAllPlan(true)}
        canDeploy={false}
        deploymentEvidence={{
          id: "deployment-1",
          status: "queued",
          token: "deployment-1",
          nodes: [],
        }}
        commitMessage=""
        canSaveWithoutDeploying
        onCommitMessageChange={vi.fn()}
        onDeploy={vi.fn()}
        onSaveWithoutDeploying={vi.fn()}
        onDiscardAll={vi.fn()}
        onDiscardNode={vi.fn()}
        onDiscardRow={vi.fn()}
      />,
    );

    expect(screen.queryByRole("button", { name: "Deploy" })).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Details" }));
    expect(screen.getByText("1 unsaved")).toBeTruthy();
    expect(screen.getByText("2 pending")).toBeTruthy();
    expect(screen.getByText("3 drift")).toBeTruthy();
    expect(screen.getAllByText("Deployment queued")).toHaveLength(2);
    expect(
      screen.queryByRole("button", { name: "Deploy changes" }),
    ).toBeNull();
  });

  it("keeps deployment evidence visible when the canonical Change Set is empty", () => {
    render(
      <ApplyChangesBar
        groups={[]}
        totalChanges={0}
        discardAllPlan={discardAllPlan(false)}
        deploymentEvidence={{
          id: "deployment-1",
          status: "deploying",
          token: "deployment-1",
          nodes: [],
        }}
        commitMessage=""
        canSaveWithoutDeploying={false}
        onCommitMessageChange={vi.fn()}
        onDeploy={vi.fn()}
        onSaveWithoutDeploying={vi.fn()}
        onDiscardAll={vi.fn()}
        onDiscardNode={vi.fn()}
        onDiscardRow={vi.fn()}
      />,
    );

    expect(screen.getByText("Deployment deploying")).toBeTruthy();
  });
});
