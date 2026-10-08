import { useEffect, useState, type FormEvent, type KeyboardEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchServerSentEvents, useChat, type UIMessage } from "@tanstack/ai-react";
import { Option, Schema } from "effect";
import { ArrowUpIcon, SquarePenIcon, XIcon } from "lucide-react";
import type { CollectionScope } from "#/collections/scope";
import { Bubble, BubbleContent } from "#/components/ui/bubble";
import { Button } from "#/components/ui/button";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupTextarea } from "#/components/ui/input-group";
import {
  MessageScroller, MessageScrollerButton, MessageScrollerContent, MessageScrollerItem, MessageScrollerProvider, MessageScrollerViewport,
} from "#/components/ui/message-scroller";
import { approvalInterrupt } from "#/modules/agent/agent";
import { ApprovalCard } from "./approval-card";
import { pendingApprovalsOptions } from "./approvals.queries";
import { toolOutcome, ToolRow } from "./tool-row";

const threadKey = (organizationSlug: string) => `ployz.agent.thread.${organizationSlug}`;
const askedKey = (threadId: string) => `ployz.agent.asked.${threadId}`;
const Asked = Schema.fromJsonString(Schema.Record(Schema.String, Schema.String));
type Asked = typeof Asked.Type;

function storedThread(organizationSlug: string) {
  const stored = localStorage.getItem(threadKey(organizationSlug));
  if (stored) return stored;
  const fresh = crypto.randomUUID();
  localStorage.setItem(threadKey(organizationSlug), fresh);
  return fresh;
}

/** The Organization's one agent conversation, kept across reloads; "New chat" starts another. */
export default function AgentPanel({ organizationSlug, environment, scope, onClose }: {
  organizationSlug: string;
  environment: string | null;
  scope: CollectionScope;
  onClose: () => void;
}) {
  const [threadId, setThreadId] = useState(() => storedThread(organizationSlug));
  const newChat = () => {
    const fresh = crypto.randomUUID();
    localStorage.setItem(threadKey(organizationSlug), fresh);
    setThreadId(fresh);
  };
  return (
    <section aria-label="Ployz agent" className="flex h-full min-h-0 flex-col bg-background">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b px-3">
        <h2 className="text-sm font-semibold">Ployz agent</h2>
        <span className="truncate text-xs text-muted-foreground">{environment ? `${organizationSlug} / ${environment}` : organizationSlug}</span>
        <Button className="ml-auto" size="icon-sm" variant="ghost" aria-label="New chat" title="New chat" onClick={newChat}><SquarePenIcon /></Button>
        <Button size="icon-sm" variant="ghost" aria-label="Close agent" title="Close" onClick={onClose}><XIcon /></Button>
      </header>
      <Conversation key={threadId} organizationSlug={organizationSlug} scope={scope} threadId={threadId} />
    </section>
  );
}

function Conversation({ organizationSlug, scope, threadId }: { organizationSlug: string; scope: CollectionScope; threadId: string }) {
  const [connection] = useState(() => fetchServerSentEvents(`/api/agent/${encodeURIComponent(organizationSlug)}/chat`));
  const chat = useChat({ connection, threadId, persistence: true, interrupts: [approvalInterrupt], live: true });
  const bound = chat.interrupts.flatMap((interrupt) =>
    interrupt.kind === "generic" && "definitionId" in interrupt && interrupt.definitionId === approvalInterrupt.id && interrupt.payload
      ? [{ key: interrupt.key, approvalId: interrupt.payload.approvalId, interrupt }]
      : []);

  const [asked, setAsked] = useState<Asked>(() =>
    Option.getOrElse(Schema.decodeUnknownOption(Asked)(localStorage.getItem(askedKey(threadId))), () => ({})));
  const unseen = bound.filter(({ key, approvalId }) => asked[key] !== approvalId);
  if (unseen.length > 0) setAsked({ ...asked, ...Object.fromEntries(unseen.map(({ key, approvalId }) => [key, approvalId])) });
  useEffect(() => { localStorage.setItem(askedKey(threadId), JSON.stringify(asked)); }, [threadId, asked]);

  const { data: pending } = useQuery(pendingApprovalsOptions(organizationSlug));
  const ours = new Set(Object.values(asked));
  const waiting = (pending ?? []).filter((approval) => !ours.has(approval.id));

  return (
    <MessageScrollerProvider>
      <MessageScroller className="flex-1">
        <MessageScrollerViewport>
          <MessageScrollerContent className="gap-3 p-3">
            {waiting.length > 0 && (
              <MessageScrollerItem>
                <section aria-label="Waiting on you" className="flex flex-col gap-2">
                  <h3 className="text-xs font-medium text-muted-foreground">Waiting on you</h3>
                  {waiting.map((approval) => <ApprovalCard key={approval.id} organizationSlug={organizationSlug} id={approval.id} seed={approval} />)}
                </section>
              </MessageScrollerItem>
            )}
            {chat.messages.length === 0 && waiting.length === 0 && !chat.isHydrating && (
              <p className="text-sm text-muted-foreground">Ask about your Projects, Deployments and Servers, or have the agent deploy for you. It stops to ask before anything is destroyed.</p>
            )}
            {chat.messages.map((message) => (
              <MessageScrollerItem key={message.id} scrollAnchor={message.role === "user"}>
                <Message organizationSlug={organizationSlug} scope={scope} message={message} asked={asked} bound={bound} />
              </MessageScrollerItem>
            ))}
            {chat.error && <p role="alert" className="text-sm text-destructive">{chat.error.message}</p>}
          </MessageScrollerContent>
        </MessageScrollerViewport>
        <MessageScrollerButton />
      </MessageScroller>
      <Composer busy={chat.isLoading} onSend={(text) => void chat.sendMessage(text)} />
    </MessageScrollerProvider>
  );
}

type Bound = { key: string; approvalId: string; interrupt: { canResolve: boolean; status: string; resolveInterrupt: (response: Record<string, never>) => void } };

function Message({ organizationSlug, scope, message, asked, bound }: {
  organizationSlug: string;
  scope: CollectionScope;
  message: UIMessage;
  asked: Asked;
  bound: readonly Bound[];
}) {
  if (message.role === "user") {
    const text = message.parts.flatMap((part) => part.type === "text" ? [part.content] : []).join("\n");
    return <Bubble align="end" variant="secondary"><BubbleContent className="whitespace-pre-wrap">{text}</BubbleContent></Bubble>;
  }
  const results = new Map(message.parts.flatMap((part) =>
    part.type === "tool-result" ? [[part.toolCallId, part] as const] : []));
  return (
    <div className="flex flex-col gap-2 text-sm">
      {message.parts.map((part, index) => {
        if (part.type === "text") return <p key={index} className="whitespace-pre-wrap">{part.content}</p>;
        if (part.type !== "tool-call") return null;
        const waitingOn = bound.find(({ key }) => key === part.id);
        const approvalId = waitingOn?.approvalId ?? asked[part.id];
        const outcome = toolOutcome(part, results.get(part.id));
        const settle = waitingOn && (() => {
          if (waitingOn.interrupt.canResolve && waitingOn.interrupt.status === "pending") waitingOn.interrupt.resolveInterrupt({});
        });
        return (
          <div key={part.id} className="flex flex-col gap-2">
            {approvalId && <ApprovalCard organizationSlug={organizationSlug} id={approvalId} autoFocus={waitingOn !== undefined} onSettled={settle} />}
            {(!approvalId || Option.isSome(outcome)) && (
              <ToolRow organizationSlug={organizationSlug} scope={scope} name={part.name} outcome={outcome}
                done={part.state === "complete" || results.has(part.id)} />
            )}
          </div>
        );
      })}
    </div>
  );
}

function Composer({ busy, onSend }: { busy: boolean; onSend: (text: string) => void }) {
  const [draft, setDraft] = useState("");
  const send = (event?: FormEvent) => {
    event?.preventDefault();
    const text = draft.trim();
    if (!text || busy) return;
    onSend(text);
    setDraft("");
  };
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) send(event);
  };
  return (
    <form onSubmit={send} className="shrink-0 border-t p-2">
      <InputGroup>
        <InputGroupTextarea aria-label="Message the agent" placeholder="Ask about your servers…" rows={1} value={draft}
          onChange={(event) => setDraft(event.target.value)} onKeyDown={onKeyDown} className="max-h-40 min-h-9" />
        <InputGroupAddon align="inline-end">
          <InputGroupButton type="submit" size="icon-xs" variant="default" aria-label="Send" disabled={busy || !draft.trim()}>
            <ArrowUpIcon />
          </InputGroupButton>
        </InputGroupAddon>
      </InputGroup>
    </form>
  );
}
