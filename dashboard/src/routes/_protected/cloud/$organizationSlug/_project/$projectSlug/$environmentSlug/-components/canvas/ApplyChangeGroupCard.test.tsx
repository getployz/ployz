// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { ChangeGroup } from "#/modules/config-store/store-deployments";
import { ApplyChangeGroupCard } from "./ApplyChangeGroupCard";

const lifecycleOnlyGroup: ChangeGroup = {
  nodeType: "service",
  nodeId: "00000000-0000-4000-8000-000000000001",
  nodeName: "nginx",
  discardPath: "nginx",
  changeCount: 1,
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

  it("offers Never sync beside Discard on a row that arrived, and closes as the last change goes", () => {
    const [onCloseDialog, neverSync] = [vi.fn(), vi.fn()];
    const row = (path: string, label: string) => ({
      changeKey: `service:api:${path}`, label, kind: "update" as const, path, currentValue: "60", newValue: "300", canDiscard: true,
    });
    render(
      <ApplyChangeGroupCard
        group={{ ...lifecycleOnlyGroup, nodeName: "api", discardPath: "api", lifecycle: "update",
          rows: [row("api.env.CACHE_TTL", "Environment variable CACHE_TTL"), row("api.replicas", "Replicas")] }}
        totalChanges={1}
        visibleGroupCount={1}
        onCloseDialog={onCloseDialog}
        onDiscardNode={vi.fn()}
        onDiscardRow={vi.fn()}
        noteFor={(path) => path === "api.env.CACHE_TTL" ? "From production" : null}
        neverSyncFor={(path) => path === "api.env.CACHE_TTL" ? neverSync : undefined}
      />,
    );

    expect(screen.getByText("From production")).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /^Never sync/u })).toHaveLength(1);
    expect(screen.getByRole("button", { name: "Discard Environment variable CACHE_TTL" })).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Never sync Environment variable CACHE_TTL" }));
    expect(neverSync).toHaveBeenCalledOnce();
    expect(onCloseDialog).toHaveBeenCalledOnce();
  });
});
