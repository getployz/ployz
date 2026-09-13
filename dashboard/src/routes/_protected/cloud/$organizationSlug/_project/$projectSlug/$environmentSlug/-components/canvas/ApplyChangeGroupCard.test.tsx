// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { CanvasNodeDiffGroup } from "#/modules/environment-design/canvas-node-diff";
import { ApplyChangeGroupCard } from "./ApplyChangeGroupCard";

const lifecycleOnlyGroup: CanvasNodeDiffGroup = {
  nodeType: "service",
  nodeId: "00000000-0000-4000-8000-000000000001",
  nodeName: "nginx",
  summaryLabel: "nginx",
  serviceSourceType: "image",
  lifecycle: "create",
  canDiscard: true,
  rows: [],
};

afterEach(cleanup);

describe("ApplyChangeGroupCard", () => {
  it("renders a new unchanged node as lifecycle-only", () => {
    const onDiscardNode = vi.fn();
    render(
      <ApplyChangeGroupCard
        group={lifecycleOnlyGroup}
        totalChanges={1}
        visibleGroupCount={1}
        onCloseDialog={vi.fn()}
        onDiscardNode={onDiscardNode}
        onDiscardRow={vi.fn()}
      />,
    );

    expect(screen.getByText("will be added")).toBeTruthy();
    expect(screen.queryByText(/Settings/u)).toBeNull();
    expect(screen.queryByText("Change")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(onDiscardNode).toHaveBeenCalledWith(lifecycleOnlyGroup);
  });

  it("labels only the settings added beyond creation", () => {
    render(
      <ApplyChangeGroupCard
        group={{
          ...lifecycleOnlyGroup,
          rows: [
            {
              changeKey: "service:00000000-0000-4000-8000-000000000001:source.branch",
              label: "Branch",
              kind: "update",
              path: "source.branch",
              currentValue: "main",
              newValue: "master",
              canDiscard: true,
            },
          ],
        }}
        totalChanges={2}
        visibleGroupCount={1}
        onCloseDialog={vi.fn()}
        onDiscardNode={vi.fn()}
        onDiscardRow={vi.fn()}
      />,
    );

    expect(screen.getByText("1 Setting")).toBeTruthy();
    expect(screen.getByText("Branch")).toBeTruthy();
    expect(screen.queryByText("Pending")).toBeNull();
  });
});
