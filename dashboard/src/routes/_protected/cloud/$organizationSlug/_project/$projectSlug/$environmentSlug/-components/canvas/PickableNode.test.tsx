// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ReactFlowProvider } from "@xyflow/react";
import { afterEach, expect, it, vi } from "vitest";
import type { NodePick } from "../new-branch/branch-picking";
import { LiveLabel, PickableNode } from "./PickableNode";

afterEach(cleanup);

function card(pick: NodePick, name: string) {
  render(
    <ReactFlowProvider>
      <PickableNode pick={pick} name={name} nodeId={name}>
        <LiveLabel label={pick.label} ownsData={pick.ownsData} />
      </PickableNode>
    </ReactFlowProvider>,
  );
}

it("announces an Own Copy as pressed and toggles on click", () => {
  const toggle = vi.fn();
  card({ role: "own", label: "Own copy", fixed: false, ownsData: false, toggle }, "api");
  const api = screen.getByRole("button", { name: "api, Own copy", pressed: true });
  fireEvent.click(api);
  expect(toggle).toHaveBeenCalledOnce();
});

it("shows a Live Node that owns data as whose it is, with real data", () => {
  card({ role: "live", label: "production's, live", fixed: false, ownsData: true, toggle: vi.fn() }, "postgres");
  expect(screen.getByRole("button", { name: "postgres, production's, live, real data", pressed: false })).toBeTruthy();
  expect(screen.getByText("real data")).toBeTruthy();
});

it("changes nothing when another copy needs the node", () => {
  const toggle = vi.fn();
  card({ role: "own", label: "Own copy", fixed: true, ownsData: false, toggle }, "pg-data");
  const data = screen.getByRole("button", { name: "pg-data, Own copy" });
  expect(data.getAttribute("aria-disabled")).toBe("true");
  fireEvent.click(data);
  expect(toggle).not.toHaveBeenCalled();
});
