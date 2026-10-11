import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { SparklesIcon } from "lucide-react";
import type { CollectionScope } from "#/collections/scope";
import type { DashboardScope } from "#/components/dashboard-navigation-model";
import type { PageContext } from "#/modules/agent/page-context";
import { Badge } from "#/components/ui/badge";
import { cn } from "#/lib/utils";
import { pendingApprovalsOptions } from "./approvals.queries";

type Panel = typeof import("./agent-panel").default;
const loadPanel = () => import("./agent-panel").then((module) => module.default);

const threadKey = (organizationSlug: string) => `ployz.agent.thread.${organizationSlug}`;

/**
 * The agent, on every Organization page: a tab on the right edge that counts approvals waiting on a human, opening into
 * the conversation. Over the canvas it floats above the graph instead of squeezing it.
 *
 * `chat` is the open thread, kept in the URL by the Organization route, so a reload or a shared link lands in the same conversation. Closing drops it;
 * opening again returns to the Organization's last thread, remembered in localStorage.
 *
 * The panel loads on first hover or open and renders as soon as it arrives. A lazy Suspense boundary would hold it back
 * for React's 300 ms reveal throttle.
 */
export function AgentSidebar({ scope, collectionScope, page, canvas, chat, onChat }: {
  scope: DashboardScope;
  collectionScope: CollectionScope;
  page: PageContext;
  canvas: boolean;
  chat: string | undefined;
  onChat: (thread: string | undefined) => void;
}) {
  const { organizationSlug } = scope;
  const [AgentPanel, setAgentPanel] = useState<Panel | null>(null);
  const { data: pending } = useQuery(pendingApprovalsOptions(organizationSlug));
  const waiting = pending?.length ?? 0;
  const openPanel = () => onChat(localStorage.getItem(threadKey(organizationSlug)) ?? crypto.randomUUID());

  useEffect(() => {
    if (chat === undefined) return;
    localStorage.setItem(threadKey(organizationSlug), chat);
    void loadPanel().then((panel) => setAgentPanel(() => panel));
  }, [organizationSlug, chat]);

  if (chat === undefined) {
    return (
      <button type="button" onClick={openPanel} onPointerEnter={() => void loadPanel()}
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
      {AgentPanel && (
        <AgentPanel organizationSlug={organizationSlug} environment={scope.kind === "environment" ? scope.environmentSlug : null}
          scope={collectionScope} page={page} threadId={chat} onNewChat={() => onChat(crypto.randomUUID())} onClose={() => onChat(undefined)} />
      )}
    </aside>
  );
}
