import { useQueryClient } from "@tanstack/react-query";
import { useParams, useRouteContext } from "@tanstack/react-router";

export function useCollectionScope() {
  const queryClient = useQueryClient();
  const { session } = useRouteContext({ from: "/_protected" });
  const environmentSlug = useParams({
    from: "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug",
    shouldThrow: false,
  })?.environmentSlug;
  return { queryClient, environmentSlug, sessionId: session.session.id, userId: session.user.id };
}
