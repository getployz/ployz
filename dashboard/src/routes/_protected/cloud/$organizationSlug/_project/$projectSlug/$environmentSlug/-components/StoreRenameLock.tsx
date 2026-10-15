import type { ReactNode } from "react";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { useStoreRenamePending } from "#/modules/config-store/store-pending";
import { cn } from "#/lib/utils";
import { ENVIRONMENT_ROUTE_FROM } from "./environment-route-paths";

/** Whether a rename in this Environment is saving. Any edit can name the renamed node, in its path or its value, so all of them wait. */
export function useEnvironmentRenaming(): boolean {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  return useStoreRenamePending(organizationSlug, store);
}

/** Locks everything it holds while a rename in this Environment is saving. */
export function StoreRenameLock({ className, children }: { className?: string; children: ReactNode }) {
  const renaming = useEnvironmentRenaming();
  return <div inert={renaming} aria-busy={renaming} className={cn("aria-busy:opacity-60", className)}>{children}</div>;
}
