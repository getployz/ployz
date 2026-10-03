// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { JsonValue, ServiceSettingChange, SettingRow } from "@ployz/sdk";
import * as scopes from "#/collections/use-collection-scope";
import { asTestDouble } from "#/lib/test-double";
import * as functions from "#/modules/config-store/store.functions";
import { StoreSourceSection, type StoreService } from "./StoreServiceDrawer";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("notes an image change with what was deployed, and Undo discards it", () => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue({ queryClient, sessionId: "session", userId: "user" });
  // The PR plans an image never reads.
  vi.spyOn(functions, "readStoreViewServerFn").mockReturnValue(new Promise(() => {}));
  const discard = vi.fn();
  const change = (before: JsonValue, after: JsonValue) => asTestDouble<ServiceSettingChange>()({ path: "web.source", before, after });
  const state = asTestDouble<StoreService>()({
    organizationSlug: "acme",
    environment: { project: "shop", environment: "production" },
    source: "image",
    service: asTestDouble<StoreService["service"]>()({ template: null }),
    rows: new Map([["image", asTestDouble<SettingRow>()({ value: "web:2" })]]),
    changes: new Map([
      ["source", change({ type: "image", image: "web:1", credentials: false }, { type: "image", image: "web:2", credentials: false })],
      ["image", change("web:1", "web:2")],
    ]),
    discard,
  });
  render(<QueryClientProvider client={queryClient}><StoreSourceSection state={state} /></QueryClientProvider>);
  expect(screen.getByText("Was web:1")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Undo" }));
  expect(discard).toHaveBeenCalledWith("image");
});
