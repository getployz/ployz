// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import * as runtime from "#/providers/runtime-provider";
import { SSH_UPGRADE, ServersUnreachable } from "./servers-unreachable";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function renderAs(role: "admin" | "member" | "support", older = false) {
  vi.spyOn(runtime, "useClusterReleased").mockReturnValue(() => {});
  const queryClient = new QueryClient();
  queryClient.setQueryData(organizationMembersQueryOptions("ada").queryKey, {
    members: role === "support" ? [] : [{ name: "Linus", email: "linus@example.com", role, joined_at: "2026-01-01T00:00:00Z", you: true }],
    invites: [],
    mail: false,
    support: role === "support",
  });
  render(<QueryClientProvider client={queryClient}><ServersUnreachable organizationSlug="ada" older={older} /></QueryClientProvider>);
  return screen.queryByRole("button", { name: "Forget them and start over." });
}

it("lets an admin forget the servers", () => {
  expect(renderAs("admin")?.hasAttribute("disabled")).toBe(false);
});

it("disables forgetting for a member, and says why", () => {
  expect(renderAs("member")?.hasAttribute("disabled")).toBe(true);
  expect(screen.getByText(/Only an owner or admin can forget them\./)).toBeTruthy();
});

it("lets support forget the servers, as Cloud does", () => {
  expect(renderAs("support")?.hasAttribute("disabled")).toBe(false);
});

it("points a Server on 0.3.0-beta.6 or earlier to its upgrade over SSH, not to Forget", () => {
  expect(renderAs("admin", true)).toBeNull();
  expect(screen.getByText("Upgrade your server over SSH")).toBeTruthy();
  expect(screen.getByText(/runs Ployz 0\.3\.0-beta\.6 or earlier/)).toBeTruthy();
  expect(screen.getByText((_, element) => element?.tagName === "CODE" && element.textContent === SSH_UPGRADE)).toBeTruthy();
});
