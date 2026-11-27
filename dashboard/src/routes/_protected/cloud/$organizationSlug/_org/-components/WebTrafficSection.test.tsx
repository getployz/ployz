// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import * as doors from "#/modules/ingress/doors.functions";
import * as ingress from "#/modules/ingress/doors.queries";
import * as members from "#/modules/organization/members.queries";
import { WebTrafficSection } from "./WebTrafficSection";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("keeps the setup form through Escape while setup runs, and shows why it failed there", async () => {
  vi.spyOn(ingress, "useClusterIngress").mockReturnValue(asTestDouble<ReturnType<typeof ingress.useClusterIngress>>()({ setup: null, change: null, foundedClosed: false }));
  vi.spyOn(members, "useMemberGate").mockReturnValue({ allowed: true, reason: undefined });
  let answerZones = (_zones: string[]) => {};
  let refuseSetUp = (_error: Error) => {};
  vi.spyOn(doors, "listCloudflareZonesServerFn").mockImplementation(() => new Promise((resolve) => { answerZones = (zones) => resolve({ zones }); }));
  vi.spyOn(doors, "setUpCloudflareServerFn").mockImplementation(() => new Promise((_, reject) => { refuseSetUp = reject; }));
  render(<QueryClientProvider client={new QueryClient()}><WebTrafficSection organizationSlug="acme" /></QueryClientProvider>);

  fireEvent.click(screen.getByRole("button", { name: "Set up" }));
  fireEvent.change(screen.getByLabelText("Cloudflare API token"), { target: { value: "cf-token" } });
  await act(async () => { fireEvent.submit(screen.getByLabelText("Cloudflare API token").closest("form") as HTMLFormElement); });
  await act(async () => { answerZones(["example.com"]); });
  expect(screen.getByLabelText<HTMLInputElement>("Zone").value).toBe("example.com");

  await act(async () => { fireEvent.submit(screen.getByLabelText("Zone").closest("form") as HTMLFormElement); });
  // The Zone input keeps focus while setup runs, so Escape reaches the form.
  fireEvent.keyDown(screen.getByLabelText("Zone"), { key: "Escape" });
  await act(async () => { refuseSetUp(new Error("Cloudflare refused the token.")); });
  expect(screen.getByRole("alert").textContent).toBe("Cloudflare refused the token.");

  fireEvent.keyDown(screen.getByLabelText("Zone"), { key: "Escape" });
  expect(screen.queryByLabelText("Cloudflare API token")).toBeNull();
});
