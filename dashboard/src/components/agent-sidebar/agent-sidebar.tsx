import { lazy, Suspense, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { SparklesIcon } from "lucide-react";
import type { CollectionScope } from "#/collections/scope";
import type { DashboardScope } from "#/components/dashboard-navigation-model";
import { Badge } from "#/components/ui/badge";
import { cn } from "#/lib/utils";
import { pendingApprovalsOptions } from "./approvals.queries";

const AgentPanel = lazy(() => import("./agent-panel"));

/**
 * The agent, on every Organization page: a tab on the right edge that counts approvals waiting on a human, opening into
 * the conversation. Over the canvas it floats above the graph instead of squeezing it.
 */
export function AgentSidebar({ scope, collectionScope, canvas }: { scope: DashboardScope; collectionScope: CollectionScope; canvas: boolean }) {
  const [open, setOpen] = useState(false);
  const { data: pending } = useQuery(pendingApprovalsOptions(scope.organizationSlug));
  const waiting = pending?.length ?? 0;

  if (!open) {
    return (
      <button type="button" onClick={() => setOpen(true)}
        aria-label={waiting > 0 ? `Open agent, ${waiting} ${waiting === 1 ? "approval" : "approvals"} waiting` : "Open agent"}
        className="absolute top-1/2 right-0 z-30 flex -translate-y-1/2 flex-col items-center gap-1.5 rounded-l-lg border border-r-0 bg-background px-1.5 py-2.5 text-muted-foreground shadow-sm outline-none hover:text-foreground focus-visible:ring-3 focus-visible:ring-ring/50">
        <SparklesIcon aria-hidden className="size-4" />
        {waiting > 0 && <Badge variant="destructive" aria-hidden>{waiting}</Badge>}
      </button>
    );
  }
  return (
    <aside className={cn("z-30 flex min-h-0 w-full flex-col border-l bg-background sm:w-96",
      canvas ? "absolute inset-y-0 right-0 shadow-lg" : "max-sm:absolute max-sm:inset-y-0 max-sm:right-0 sm:shrink-0")}>
      <Suspense fallback={null}>
        <AgentPanel organizationSlug={scope.organizationSlug} environment={scope.kind === "environment" ? scope.environmentSlug : null}
          scope={collectionScope} onClose={() => setOpen(false)} />
      </Suspense>
    </aside>
  );
}
