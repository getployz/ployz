// @vitest-environment jsdom
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { cleanup, renderHook } from "@testing-library/react";
import type { ReactNode } from "react";
import { afterEach, expect, it, vi } from "vitest";
import * as scopes from "#/collections/use-collection-scope";
import * as functions from "#/modules/config-store/store.functions";
import { environmentsQuery, useCachedStoreView } from "#/modules/config-store/store-view.queries";
import { requireStoreEnvironment } from "./route-data";

afterEach(() => { cleanup(); vi.restoreAllMocks(); });

it("opens an Environment whose slow read a breadcrumb stopped watching", async () => {
  const queryClient = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const scope = { queryClient, sessionId: "session", userId: "user" };
  vi.spyOn(scopes, "useCollectionScope").mockReturnValue(scope);
  const listing = { ok: true, value: { view: "environments", environments: [{ id: "env", name: "production" }] } };
  const reads: Array<(result: typeof listing) => void> = [];
  vi.spyOn(functions, "readStoreViewServerFn").mockImplementation(() => new Promise((answer) => { reads.push(answer); }) as never);

  const opening = requireStoreEnvironment({ queryClient, session: { session: { id: "session" }, user: { id: "user" } } },
    { organizationSlug: "acme", projectSlug: "shop", environmentSlug: "production" });
  // The new URL's breadcrumb watches the same read, then unmounts while the page is still pending.
  const wrapper = ({ children }: { children: ReactNode }) => <QueryClientProvider client={queryClient}>{children}</QueryClientProvider>;
  renderHook(() => useCachedStoreView("acme", environmentsQuery("shop")), { wrapper }).unmount();
  reads[0]?.(listing);

  await expect(opening).resolves.toMatchObject({ id: "env", name: "production" });
});
