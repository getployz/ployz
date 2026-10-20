// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import { ServersUnreachable } from "./servers-unreachable";

afterEach(cleanup);

function renderAs(role: "admin" | "member" | "support") {
  const queryClient = new QueryClient();
  queryClient.setQueryData(organizationMembersQueryOptions("ada").queryKey, {
    members: role === "support" ? [] : [{ name: "Linus", email: "linus@example.com", role, joined_at: "2026-01-01T00:00:00Z", you: true }],
    invites: [],
    mail: false,
    support: role === "support",
  });
  render(<QueryClientProvider client={queryClient}><ServersUnreachable organizationSlug="ada" /></QueryClientProvider>);
  return screen.getByRole("button", { name: "Forget them and start over." });
}

it("lets an admin forget the servers", () => {
  expect(renderAs("admin").hasAttribute("disabled")).toBe(false);
});

it("disables forgetting for a member, and says why", () => {
  expect(renderAs("member").hasAttribute("disabled")).toBe(true);
  expect(screen.getByText(/Only an owner or admin can forget them\./)).toBeTruthy();
});

it("lets support forget the servers, as Cloud does", () => {
  expect(renderAs("support").hasAttribute("disabled")).toBe(false);
});
