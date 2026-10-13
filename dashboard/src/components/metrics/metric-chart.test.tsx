// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { createMemoryHistory, createRootRoute, createRouter, RouterProvider } from "@tanstack/react-router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { MetricChart, useMetricCursor } from "./metric-chart";

beforeEach(() => vi.stubGlobal("ResizeObserver", class { observe() {} unobserve() {} disconnect() {} }));
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

const from = Date.UTC(2026, 9, 13, 3);
const to = from + 60 * 60_000;
const deployAt = from + 30 * 60_000;

function Chart() {
  const cursor = useMetricCursor();
  return (
    <MetricChart id="cpu" title="CPU" value="" format={String} cursor={cursor} from={from} to={to}
      times={[from, to]} series={[{ label: "CPU", color: "red", values: [1, 1] }]}
      gaps={[{ from: deployAt - 10 * 60_000, to: deployAt + 10 * 60_000 }]} deploys={[deployAt]}
      markers={[{ at: deployAt, kind: "restart", label: "Restarted" }, { at: deployAt, kind: "oom", label: "Out of memory" }]} />
  );
}

async function open() {
  const root = createRootRoute({ component: Chart, loader: () => ({ timeZone: "UTC" }) });
  const router = createRouter({ routeTree: root, history: createMemoryHistory() });
  render(<RouterProvider router={router} />);
  await screen.findByText("Not measured");
}

it("draws a deploy over the hatch of a gap it falls in", async () => {
  await open();
  const hatch = screen.getByText("Not measured").parentElement;
  const deploy = document.querySelector("[data-deploy]");
  expect(deploy).not.toBeNull();
  expect((hatch?.compareDocumentPosition(deploy as Node) ?? 0) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
});

it("raises an OOM above a restart at the same moment", async () => {
  await open();
  const top = (label: string) => parseFloat(screen.getByRole("img", { name: label }).style.top);
  expect(top("Out of memory")).toBeLessThan(top("Restarted"));
});
