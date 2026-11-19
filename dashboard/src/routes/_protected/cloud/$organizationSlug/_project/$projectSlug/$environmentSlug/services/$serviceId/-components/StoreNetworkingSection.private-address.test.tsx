// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { RowId } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import * as pending from "#/modules/config-store/store-pending";
import * as views from "#/modules/config-store/store-view.queries";
import * as writes from "#/modules/config-store/store-write";
import type { StoreService } from "./StoreServiceDrawer";
import { StoreNetworkingSection } from "./StoreNetworkingSection";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

function renderSection(read: () => ReturnType<typeof views.useStoreView>) {
  vi.spyOn(views, "useStoreView").mockImplementation(read);
  vi.spyOn(pending, "useStorePendingNodes").mockReturnValue(
    asTestDouble<ReturnType<typeof pending.useStorePendingNodes>>()({ leaving: { domain: new Set<string>() } }));
  vi.spyOn(pending, "useStorePendingDomains").mockReturnValue([]);
  vi.spyOn(writes, "useStoreWriter").mockReturnValue(asTestDouble<ReturnType<typeof writes.useStoreWriter>>()({ commit: vi.fn() }));
  const state = asTestDouble<StoreService>()({
    organizationSlug: "ada",
    environment: { project: "shop", environment: "production" },
    service: { id: "web-id", row: "web-id:node" as RowId, name: "web", private_dns: "web", source: "image", change: null, template: null },
    rows: new Map(),
    changes: new Map(),
    path: (name: string) => `web.${name}`,
    set: vi.fn(),
  });
  render(<StoreNetworkingSection state={state} validatePrivateDns={() => null} />);
  const address = screen.getByLabelText<HTMLInputElement>("Private address");
  address.focus();
  fireEvent.change(address, { target: { value: "web-v2" } });
  return () => screen.getByLabelText<HTMLInputElement>("Private address");
}

it("keeps a private address being typed when the domains land", async () => {
  let land = () => {};
  const domains = new Promise<void>((resolve) => { land = resolve; });
  let landed = false;
  const address = renderSection(() => {
    // As `useSuspenseQuery` does while the Cluster hasn't answered.
    if (!landed) throw domains;
    return asTestDouble<ReturnType<typeof views.useStoreView>>()({ ok: true, value: { domains: [] } });
  });
  await act(async () => {
    landed = true;
    land();
    await domains;
  });

  expect(await screen.findByText("No public domains")).toBeTruthy();
  expect(address().value).toBe("web-v2");
});

it("keeps a private address being typed when the domains fail", async () => {
  vi.spyOn(console, "error").mockImplementation(() => {});
  let fail = () => {};
  const domains = new Promise<void>((resolve) => { fail = resolve; });
  let failed = false;
  const address = renderSection(() => {
    // As `useSuspenseQuery` does once the Cluster's read rejects.
    if (failed) throw new Error("The Cluster didn't answer");
    throw domains;
  });
  await act(async () => {
    failed = true;
    fail();
    await domains;
  });

  expect(await screen.findByRole("button", { name: "Retry" })).toBeTruthy();
  expect(address().value).toBe("web-v2");
});
