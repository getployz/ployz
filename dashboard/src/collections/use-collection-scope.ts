import { useQueryClient } from "@tanstack/react-query";
import { useRouteContext } from "@tanstack/react-router";
import type { CollectionScope } from "./scope";

export function useCollectionScope(): CollectionScope {
  const queryClient = useQueryClient();
  const { session } = useRouteContext({ from: "/_protected" });
  return { queryClient, sessionId: session.session.id, userId: session.user.id };
}
