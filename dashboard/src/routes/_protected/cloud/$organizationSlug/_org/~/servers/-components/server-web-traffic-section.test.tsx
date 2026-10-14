// @vitest-environment jsdom
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { ServerWebTrafficSection } from "./server-web-traffic-section";
import { WebTrafficDialog } from "./web-traffic-dialog";

afterEach(cleanup);

const section = (view: Parameters<typeof ServerWebTrafficSection>[0]["view"]) =>
  render(<ServerWebTrafficSection view={view} onTurnOn={() => {}} onAskOff={() => {}} unavailable={null} />);

it("never says your domains point at a Server with no public IP", () => {
  section({ kind: "on", address: null });
  expect(screen.getByText("No public IP yet, so your domains don’t point here.")).toBeTruthy();
  cleanup();
  section({ kind: "failed", want: "off", on: true, reason: "why", address: null });
  expect(screen.getByText("Your domains don’t point here.")).toBeTruthy();
  cleanup();
  section({ kind: "on", address: "203.0.113.10" });
  expect(screen.getByText("Your domains point at 203.0.113.10.")).toBeTruthy();
});

it("warns that more domains may stop when some weren't reported", () => {
  const dialog = (partial: boolean) =>
    render(<WebTrafficDialog serverName="web-1" domains={["a.example.com"]} partial={partial} last open onOpenChange={() => {}} onConfirm={() => {}} />);
  const warning = "Some servers haven’t reported their domains yet, so this may affect more.";
  dialog(true);
  expect(screen.getByText(warning)).toBeTruthy();
  cleanup();
  dialog(false);
  expect(screen.queryByText(warning)).toBeNull();
});
