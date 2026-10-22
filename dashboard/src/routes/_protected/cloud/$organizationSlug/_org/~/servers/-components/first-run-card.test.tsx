// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import * as fleetTokenFunctions from "#/modules/machines/waiting/fleet-token.functions";
import * as memberFunctions from "#/modules/organization/members.functions";
import { membersKey, ROLE_CHECKING, ROLE_UNCHECKED } from "#/modules/organization/members.queries";
import { FirstRunCard } from "./first-run-card";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

it("offers the install command only once your role allows it", async () => {
  let resolve!: (members: Awaited<ReturnType<typeof memberFunctions.listMembersServerFn>>) => void;
  vi.spyOn(memberFunctions, "listMembersServerFn").mockReturnValue(new Promise((settle) => { resolve = settle; }));
  render(<QueryClientProvider client={new QueryClient()}><FirstRunCard organizationSlug="ada" arrived={false} /></QueryClientProvider>);
  const get = screen.getByRole("button", { name: "Get install command" });
  expect(get).toHaveProperty("disabled", true);
  expect(get.getAttribute("title")).toBe(ROLE_CHECKING);
  expect(screen.queryByText("Only an owner or admin can add servers.")).toBeNull();
  await act(async () => resolve({ members: [{ you: true, role: "owner", name: "Ada", email: "ada@example.com", joined_at: "2026-10-01T00:00:00Z" }], invites: [], mail: false, support: false }));
  await waitFor(() => expect(get).toHaveProperty("disabled", false));
});

it("keeps the install command closed when your role can't be read", async () => {
  vi.spyOn(memberFunctions, "listMembersServerFn").mockRejectedValue(new Error("offline"));
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><FirstRunCard organizationSlug="ada" arrived={false} /></QueryClientProvider>);
  await waitFor(() => expect(client.getQueryState(membersKey("ada"))?.status).toBe("error"));
  expect(screen.getByRole("button", { name: "Get install command" })).toHaveProperty("disabled", true);
  expect(screen.getByRole("button", { name: "Get install command" }).getAttribute("title")).toBe(ROLE_UNCHECKED);
  expect(screen.queryByText("Only an owner or admin can add servers.")).toBeNull();
});

it("offers the install command on a support visit, as Cloud does", () => {
  const client = new QueryClient();
  client.setQueryData(membersKey("ada"), { members: [], invites: [], mail: false, support: true });
  render(<QueryClientProvider client={client}><FirstRunCard organizationSlug="ada" arrived={false} /></QueryClientProvider>);
  expect(screen.getByRole("button", { name: "Get install command" })).toHaveProperty("disabled", false);
});

it("makes the same 24-hour token as Add servers and ployz server token create", async () => {
  const create = vi.spyOn(fleetTokenFunctions, "createFleetTokenServerFn").mockResolvedValue({
    token: "pflt_first", id: "token-1", name: "brisk-otter", expiresAt: "2026-10-24T10:00:00Z", cloudOrigin: "https://ployz.dev", ployzVersion: "1.2.3",
  });
  const client = new QueryClient();
  client.setQueryData(membersKey("ada"), { members: [], invites: [], mail: false, support: true });
  render(<QueryClientProvider client={client}><FirstRunCard organizationSlug="ada" arrived={false} /></QueryClientProvider>);
  fireEvent.click(screen.getByRole("button", { name: "Get install command" }));
  await screen.findByText("Shown once. Expires in 24h.");
  expect(create).toHaveBeenCalledWith({ data: expect.objectContaining({ expiresIn: "1d" }) });
});
