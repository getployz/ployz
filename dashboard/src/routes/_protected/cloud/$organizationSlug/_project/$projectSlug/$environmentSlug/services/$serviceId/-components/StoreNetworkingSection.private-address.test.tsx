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

it.each([false, true])("keeps a private address being typed when the domains land (database first: %s)", async (privateFirst) => {
  let land = () => {};
  const domains = new Promise<void>((resolve) => { land = resolve; });
  let landed = false;
  vi.spyOn(views, "useStoreView").mockImplementation(() => {
    // As `useSuspenseQuery` does while the Cluster hasn't answered.
    if (!landed) throw domains;
    return asTestDouble<ReturnType<typeof views.useStoreView>>()({ ok: true, value: { domains: [] } });
  });
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
  const address = () => screen.getByLabelText<HTMLInputElement>("Private address");

  render(<StoreNetworkingSection state={state} privateFirst={privateFirst} validatePrivateDns={() => null} />);
  address().focus();
  fireEvent.change(address(), { target: { value: "web-v2" } });
  await act(async () => {
    landed = true;
    land();
    await domains;
  });

  expect(await screen.findByText("No public domains")).toBeTruthy();
  expect(address().value).toBe("web-v2");
});
