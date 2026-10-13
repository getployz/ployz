import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { SparklesIcon } from "lucide-react";
import type { CollectionScope } from "#/collections/scope";
import type { PageContext } from "#/modules/agent/page-context";
import { Badge } from "#/components/ui/badge";
import { Button } from "#/components/ui/button";
import { pendingApprovalsOptions } from "./approvals.queries";

type Panel = typeof import("./agent-panel").default;
const loadPanel = () => import("./agent-panel").then((module) => module.default);

const threadKey = (organizationSlug: string) => `ployz.agent.thread.${organizationSlug}`;

/**
 * The agent's only toggle, at the right end of the top bar. Closed, it counts the approvals waiting on a human.
 *
 * `chat` is the open thread, kept in the URL by the Organization route, so a reload or a shared link lands in the same conversation. Closing drops it;
 * opening again returns to the Organization's last thread, remembered in localStorage.
 */
export function AgentButton({ organizationSlug, chat, onChat }: {
  organizationSlug: string;
  chat: string | undefined;
  onChat: (thread: string | undefined) => void;
}) {
  const { data: pending } = useQuery(pendingApprovalsOptions(organizationSlug));
  const waiting = chat === undefined ? pending?.length ?? 0 : 0;
  const toggle = () => onChat(chat === undefined ? localStorage.getItem(threadKey(organizationSlug)) ?? crypto.randomUUID() : undefined);
  const waitingLabel = `${waiting} ${waiting === 1 ? "approval" : "approvals"} waiting`;
  return (
    <>
      {/* Polling can add an approval while focus is elsewhere, and a changed button name isn't announced. */}
      <span role="status" className="sr-only">{waiting > 0 ? waitingLabel : ""}</span>
      {/* Pressed wears the outline variant's own open look, which it keys to aria-expanded alone. */}
      <Button variant="outline" size="sm" className="aria-pressed:bg-muted aria-pressed:text-foreground" aria-pressed={chat !== undefined} onClick={toggle} onPointerEnter={() => void loadPanel()}
        aria-label={waiting > 0 ? `Agent, ${waitingLabel}` : "Agent"}>
        <SparklesIcon data-icon="inline-start" aria-hidden />
        <span className="max-wf-nav:hidden">Agent</span>
        {waiting > 0 && <Badge variant="warning">{waiting}{" "}<span className="max-wf-nav:hidden">waiting</span></Badge>}
      </Button>
    </>
  );
}

/**
 * The open agent: a column under the top bar beside the page, or the whole area above the tab bar on phones. It never
 * covers the canvas, which narrows instead.
 *
 * The panel loads on first hover or open and renders as soon as it arrives. A lazy Suspense boundary would hold it back
 * for React's 300 ms reveal throttle.
 */
export function AgentColumn({ organizationSlug, collectionScope, page, chat, onChat }: {
  organizationSlug: string;
  collectionScope: CollectionScope;
  page: PageContext;
  chat: string | undefined;
  onChat: (thread: string | undefined) => void;
}) {
  const [AgentPanel, setAgentPanel] = useState<Panel | null>(null);

  useEffect(() => {
    if (chat === undefined) return;
    localStorage.setItem(threadKey(organizationSlug), chat);
    void loadPanel().then((panel) => setAgentPanel(() => panel));
  }, [organizationSlug, chat]);

  if (chat === undefined) return null;
  return (
    <aside className="z-30 flex min-h-0 flex-col bg-background max-wf-nav:absolute max-wf-nav:inset-0 min-wf-nav:w-96 min-wf-nav:shrink-0 min-wf-nav:border-l">
      {AgentPanel && (
        <AgentPanel organizationSlug={organizationSlug} scope={collectionScope} page={page} threadId={chat}
          onChat={onChat} />
      )}
    </aside>
  );
}
