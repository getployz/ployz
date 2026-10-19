// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, render, screen } from "@testing-library/react";
import type { EnvironmentListing } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import { StoreTeardownSection } from "./store-teardown-section";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("shows a close waiting for the sweep as deleting, and can delete it again", () => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  queryClient.setQueryData<unknown>(organizationMembersQueryOptions("acme").queryKey, { members: [{ you: true, role: "owner" }] });
  const feat: EnvironmentListing = {
    id: "id-feat", name: "feat", default: false, parent: "production", merged: false, branch_setup: [], protected: false, protections: 0,
    removal: { kind: "pending", because: "synced", since: 0 },
  };
  render(
    <QueryClientProvider client={queryClient}>
      <StoreTeardownSection organizationSlug="acme" target={{ project: "shop", environment: "feat" }} environments={[feat]}
        name="feat" place="shop/feat" title="Delete feat" description="Deletes feat." actionLabel="Delete feat" headingId="danger"
        onCompleted={() => {}} />
    </QueryClientProvider>,
  );
  expect(screen.getByText("Deleting feat…")).toBeTruthy();
  expect(screen.getByText("Ployz takes it off your servers at its next hourly sweep. If that hasn't started, delete it again.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Delete feat" }).hasAttribute("disabled")).toBe(false);
});
