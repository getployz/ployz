import {
  createRouter as createTanStackRouter,
  useRouterState,
} from "@tanstack/react-router";
import { routeTree } from "./routeTree.gen";
import { setupRouterSsrQueryIntegration } from "@tanstack/react-router-ssr-query";
import { environmentManager, QueryClient } from "@tanstack/react-query";
import { NotFoundPage } from "./components/not-found-page";
import { PloyzMark } from "./components/icons/ployz-logo";
import { RouteContentSkeleton } from "./components/route-content-skeleton";

function AppPending() {
  const hasResolvedLocation = useRouterState({
    select: (state) => state.resolvedLocation !== undefined,
  });

  if (hasResolvedLocation) {
    return (
      <main className="mx-auto flex w-full max-w-6xl flex-col px-4 py-6 md:px-6 md:py-8">
        <RouteContentSkeleton />
      </main>
    );
  }

  return (
    <div
      role="status"
      aria-label="Opening Ployz"
      className="flex min-h-svh w-full flex-col items-center justify-center gap-3 bg-background"
    >
      <div className="app-boot-mark">
        <PloyzMark decorative />
      </div>
      <span className="app-boot-label text-sm text-muted-foreground">
        Opening Ployz…
      </span>
    </div>
  );
}

export function getRouter() {
  const queryClient = new QueryClient({
    defaultOptions: {
      queries: {
        // Keep SSR query reuse within a request, but make browser reads
        // authoritative unless a query opts into its own freshness window.
        staleTime: environmentManager.isServer() ? 1000 * 5 : 0,
      },
    },
  });
  const router = createTanStackRouter({
    routeTree,
    context: { queryClient },
    defaultNotFoundComponent: () => <NotFoundPage />,
    defaultPendingComponent: AppPending,
    scrollRestoration: true,
    scrollToTopSelectors: [
      '[data-scroll-restoration-id="wireframe-content"]',
    ],
    defaultPreload: "viewport",
    defaultPreloadStaleTime: 0,
    defaultPendingMs: 220,
    defaultPendingMinMs: 180,
  });

  setupRouterSsrQueryIntegration({
    router,
    queryClient,
  });

  return router;
}

declare module "@tanstack/react-router" {
  interface Register {
    router: ReturnType<typeof getRouter>;
  }
}
