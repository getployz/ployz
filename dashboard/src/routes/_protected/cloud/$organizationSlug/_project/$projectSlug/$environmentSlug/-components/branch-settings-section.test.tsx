// @vitest-environment jsdom

import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as offCommands from "#/modules/pr-environments/off-commands";
import { asTestDouble } from "#/lib/test-double";
import { ShutdownSection } from "./branch-settings-section";

afterEach(cleanup);

it("shuts a PR Environment down, waits while it shuts down, runs a failed one again, and deploys it once it's Off", () => {
  const start = vi.fn();
  const shutDown = vi.fn();
  vi.spyOn(offCommands, "usePrEnvironmentOff").mockImplementation(() => asTestDouble<ReturnType<typeof offCommands.usePrEnvironmentOff>>()({
    start: { mutate: start, isPending: false }, shutDown: { mutate: shutDown, isPending: false },
  }));
  const open = (shutdown: "running" | "off" | "failed" | null) =>
    render(<ShutdownSection organizationSlug="acme" environmentId="env-1" name="pr-142" shutdown={shutdown} />);

  open(null);
  fireEvent.click(screen.getByRole("button", { name: "Shut down pr-142" }));
  cleanup();
  open("running");
  expect(screen.getByText("Shutting down. Deploy once it's off.")).toBeTruthy();
  expect(screen.queryByRole("button")).toBeNull();
  cleanup();
  open("failed");
  expect(screen.getByText("Shutdown failed. Some services may still run.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Shut down pr-142" }));
  expect(shutDown).toHaveBeenCalledTimes(2);
  cleanup();
  open("off");
  expect(screen.getByText("Off, with its settings kept. It starts again on the next push.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Deploy pr-142" }));
  expect(start).toHaveBeenCalledOnce();
});
