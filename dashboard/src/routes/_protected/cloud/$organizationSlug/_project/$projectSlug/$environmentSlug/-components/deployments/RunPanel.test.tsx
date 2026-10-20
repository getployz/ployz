// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import type { DeploymentView } from "@ployz/sdk";
import { afterEach, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { makeRunGraph } from "./run-graph";
import { RunPanel } from "./RunPanel";

afterEach(cleanup);

it("lists a Machine the Deploy could not reach by itself, without a Service", () => {
  const deployment = asTestDouble<DeploymentView>()({
    id: "d1", status: "succeeded", in_flight: false, builds: [], nodes: [], runtime_names: {}, preview: null,
  });
  const node = makeRunGraph(deployment, {
    state: "left_running",
    left: [
      { machine: "machine-1", service: "web", reason: "machine-1 did not answer" },
      { machine: "machine-3", service: null, reason: "machine-3 was cut off during the Deploy" },
    ],
    message: null,
  }).nodes.find((found) => found.type === "cleanup");
  render(<RunPanel deployment={deployment} node={node} />);
  expect(screen.getByText("Left running on machine-1, machine-3")).toBeTruthy();
  const items = within(screen.getByRole("list", { name: "Old copies still running" })).getAllByRole("listitem");
  expect(items.map((item) => [...item.querySelectorAll("span, p")].map((part) => part.textContent))).toEqual([
    ["web", "on machine-1", "machine-1 did not answer"],
    ["machine-3", "machine-3 was cut off during the Deploy"],
  ]);
});
