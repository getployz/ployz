import { useQueryClient } from "@tanstack/react-query";
import { useRouteContext } from "@tanstack/react-router";
import { type CollectionScope, scopeOf } from "./scope";

export function useCollectionScope(): CollectionScope {
  const queryClient = useQueryClient();
  const { session } = useRouteContext({ from: "/_protected" });
  return scopeOf(queryClient, session);
}
