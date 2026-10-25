// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import type { DeploymentView } from "@ployz/sdk";
import { afterEach, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { machineId } from "#/test/ids";
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

it("never lists a copy still running under Done", () => {
  const deployment = asTestDouble<DeploymentView>()({
    id: "d1", status: "succeeded", in_flight: false, builds: [], nodes: [], runtime_names: {}, preview: null,
  });
  const node = makeRunGraph(deployment, {
    state: "done", left: [{ machine: "machine-3", service: "web", reason: "machine-3 did not answer" }], message: null,
  }).nodes.find((found) => found.type === "cleanup");
  render(<RunPanel deployment={deployment} node={node} />);
  expect(screen.getByText("Every old copy is gone")).toBeTruthy();
  expect(screen.queryByRole("list", { name: "Old copies still running" })).toBeNull();
});

it("shows a Service the Deploy removed that this Environment doesn't hold, with each Server's row and no logs", () => {
  const deployment = asTestDouble<DeploymentView>()({
    id: "d1", status: "applied", in_flight: false, builds: [], nodes: [], runtime_names: {}, preview: null,
    untargeted: {
      db: [
        { machine_id: machineId("1"), server: "fsn-1", state: "completed", started_at: 10, finished_at: 20, history: null },
        {
          machine_id: machineId("2"), server: "fsn-2", state: "failed", reason: "Docker did not answer", cause: ["connection refused"],
          step: "remove", log: null, started_at: 10, finished_at: 30, history: null,
        },
      ],
    },
  });
  render(<RunPanel deployment={deployment} node={makeRunGraph(deployment).nodes[0]} />);
  expect(screen.getByRole("heading", { name: "db" })).toBeTruthy();
  expect(screen.getByText("Failed")).toBeTruthy();
  expect(screen.getByText("Not in this environment")).toBeTruthy();
  const rows = within(screen.getByRole("list", { name: "Servers" })).getAllByRole("listitem");
  expect(rows.map((row) => row.textContent)).toEqual([
    "fsn-1Done10s",
    "fsn-2Failed at remove20sDocker did not answerconnection refused",
  ]);
});
