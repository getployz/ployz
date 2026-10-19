// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { RowId } from "@ployz/sdk";
import { afterEach, expect, it, vi } from "vitest";
import { asTestDouble } from "#/lib/test-double";
import * as pending from "#/modules/config-store/store-pending";
import * as views from "#/modules/config-store/store-view.queries";
import * as writes from "#/modules/config-store/store-write";
import { StoreRefused } from "#/modules/config-store/store.contract";
import type { StoreService } from "./StoreServiceDrawer";
import { StoreNetworkingSection } from "./StoreNetworkingSection";

afterEach(() => {
  cleanup();
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

const REFUSAL = "Expected a hostname like app.example.com";

it("reopens a refused custom domain with the Store's reason under the field until the hostname changes", async () => {
  let refuse = (_error: Error) => {};
  const write = new Promise<never>((_resolve, reject) => { refuse = reject; });
  write.catch(() => {});
  const commit = vi.fn(() => ({ isPersisted: { promise: write } }));
  vi.spyOn(views, "useStoreView").mockReturnValue(asTestDouble<ReturnType<typeof views.useStoreView>>()({}));
  vi.spyOn(views, "requireView").mockReturnValue({ domains: [] });
  vi.spyOn(pending, "useStorePendingNodes").mockReturnValue(
    asTestDouble<ReturnType<typeof pending.useStorePendingNodes>>()({ leaving: { domain: new Set<string>() } }));
  vi.spyOn(pending, "useStorePendingDomains").mockReturnValue([]);
  vi.spyOn(writes, "useStoreWriter").mockReturnValue(asTestDouble<ReturnType<typeof writes.useStoreWriter>>()({ commit }));
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
  fireEvent.click(screen.getByRole("button", { name: "Custom domain" }));
  fireEvent.change(screen.getByLabelText("Domain"), { target: { value: "under_score.рф" } });
  const save = screen.getByRole("button", { name: "Save route" });
  await waitFor(() => expect(save.hasAttribute("disabled")).toBe(false));
  fireEvent.click(save);
  await waitFor(() => expect(screen.queryByLabelText("Domain")).toBeNull());
  expect(commit).toHaveBeenCalledWith(expect.objectContaining({ command: "add_domain", hostname: "under_score.рф" }));

  await act(async () => {
    refuse(new StoreRefused({ code: "invalid_argument", message: REFUSAL, details: null }));
    await write.catch(() => {});
  });
  const domain = await screen.findByLabelText("Domain");
  expect((domain as HTMLInputElement).value).toBe("under_score.рф");
  expect(screen.getByText(REFUSAL)).toBeTruthy();

  fireEvent.change(domain, { target: { value: "under-score.рф" } });
  await waitFor(() => expect(screen.queryByText(REFUSAL)).toBeNull());
});
