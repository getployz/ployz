// @vitest-environment jsdom
import { cleanup, render, screen, within } from "@testing-library/react";
import type { DeploymentView } from "@ployz/sdk";
import { afterEach, expect, it } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import { machineId } from "#/test/ids";
import { makeRunGraph } from "#/modules/config-store/deployments/run-graph";
import { RunPanel } from "./RunPanel";

afterEach(cleanup);

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
