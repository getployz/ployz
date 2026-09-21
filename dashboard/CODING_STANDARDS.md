# Dashboard coding standards

Apply these rules when changing route loaders, collection readiness, and pending UI. Product design lives in [DESIGN.md](DESIGN.md).

## Route loading: complete SSR, responsive client navigation

Start content preloads in TanStack route loaders on both server and client. Await content readiness during SSR so the initial HTML can contain populated content. For Query-backed content, share query options between the loader and `useSuspenseQuery`; prefetch on client navigation without blocking the route. The hydrated Query cache owns readiness, including errors and pending state.

```tsx
loader: async ({ context, params }) => {
  const options = contentOptions(params, context);
  if (environmentManager.isServer()) {
    await context.queryClient.ensureQueryData(options);
  } else {
    void context.queryClient.prefetchQuery(options);
  }
}

function Content() {
  const { data } = useSuspenseQuery(contentOptions(params, context));
  return <ContentView data={data} />;
}

// Keep this boundary around the consuming region.
<Suspense fallback={<ContentSkeleton />}><Content /></Suspense>
```

Import `environmentManager` and `useSuspenseQuery` from `@tanstack/react-query`, and `Suspense` from React. Do not return an additional loader readiness promise for content already owned by Query. Use Router's `Await` for genuinely deferred loader data without a Query owner.

- Await route-critical identity, access, redirects, and not-found decisions on both server and client. Reuse cached identity on client navigation; avoid adding network work to `beforeLoad`.
- Gate only the consuming region. Keep navigation and the surrounding shell usable while page content loads. A route `pendingComponent` alone cannot cover promises returned without awaiting them.
- Start independent preloads together. Construct and preload derived collections only after their raw dependencies are ready; mount dependent live-query consumers behind that readiness boundary.
- Share Query options between loaders and component consumers when they prepare the same data. Include authenticated scope and resource scope in cache keys; reuse in-flight work and loaded collections.
- Keep version calculation and readiness inside the owning data module. Loader and render reads must derive the same query identity from hydrated collections; temporary empty live-query results must not produce a different cache key. Test the SSR-to-hydration handoff for loading fallbacks as well as mismatched HTML.
- Preserve failures. Return deferred promises to Router and let `Await` propagate rejection to an error boundary, or expose Query's error/retry state locally. Loading and failed reads are not empty collections.
- Use `useLiveSuspenseQuery` only beneath a readiness/Suspense boundary backed by a route-started preload. Ungated nested consumers use `useLiveQuery` with explicit pending/error states.
- Keep existing Query/DB hydration integration. Collection rows belong to DB serialization; do not duplicate them in loader payloads or Query dehydration.

Verify cold SSR waits, cold client navigation commits with a pending region, successful readiness reveals content, and deferred failures reach an error state. Warm navigation should reuse cached data; do not promise instant content for cold reads.
