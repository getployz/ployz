import { Fragment, useEffect, useState, type FormEvent, type KeyboardEvent } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchServerSentEvents, useChat, type UIMessage } from "@tanstack/ai-react";
import { Option, Schema } from "effect";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remend from "remend";
import { ArrowUpIcon, SquarePenIcon, XIcon } from "lucide-react";
import type { CollectionScope } from "#/collections/scope";
import { Bubble, BubbleContent } from "#/components/ui/bubble";
import { Button } from "#/components/ui/button";
import { Empty, EmptyDescription, EmptyHeader } from "#/components/ui/empty";
import { InputGroup, InputGroupAddon, InputGroupButton, InputGroupTextarea } from "#/components/ui/input-group";
import { Marker, MarkerContent, MarkerIcon } from "#/components/ui/marker";
import { Message, MessageContent } from "#/components/ui/message";
import {
  MessageScroller, MessageScrollerButton, MessageScrollerContent, MessageScrollerItem, MessageScrollerProvider, MessageScrollerViewport,
  useMessageScroller,
} from "#/components/ui/message-scroller";
import { Spinner } from "#/components/ui/spinner";
import { approvalInterrupt } from "#/modules/agent/agent";
import { ApprovalCard } from "./approval-card";
import { pendingApprovalsOptions } from "./approvals.queries";
import { toolOutcome, ToolRow } from "./tool-row";

const askedKey = (threadId: string) => `ployz.agent.asked.${threadId}`;
const Asked = Schema.fromJsonString(Schema.Record(Schema.String, Schema.String));
type Asked = typeof Asked.Type;

type PanelProps = {
  organizationSlug: string;
  environment: string | null;
  scope: CollectionScope;
  threadId: string;
  onNewChat: () => void;
  onClose: () => void;
};

/** One agent conversation; the sidebar picks the thread, and switching Organization or thread swaps in a fresh connection. */
export default function AgentPanel({ organizationSlug, environment, scope, threadId, onNewChat, onClose }: PanelProps) {
  return (
    <section aria-label="Ployz agent" className="flex h-full min-h-0 flex-col bg-background">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b px-3">
        <h2 className="text-sm font-semibold">Ployz agent</h2>
        <span className="truncate text-xs text-muted-foreground">{environment ? `${organizationSlug} / ${environment}` : organizationSlug}</span>
        <Button className="ml-auto" size="icon-sm" variant="ghost" aria-label="New chat" title="New chat" onClick={onNewChat}><SquarePenIcon /></Button>
        <Button size="icon-sm" variant="ghost" aria-label="Close agent" title="Close" onClick={onClose}><XIcon /></Button>
      </header>
      <Conversation key={`${organizationSlug}/${threadId}`} organizationSlug={organizationSlug} scope={scope} threadId={threadId} onNewChat={onNewChat} />
    </section>
  );
}

/** The client reports any refused request only as text; a 403 means the thread in the link is another member's. */
const refused = (error: Error | undefined) => error?.message.includes("status: 403") ?? false;

function Conversation({ organizationSlug, scope, threadId, onNewChat }: {
  organizationSlug: string;
  scope: CollectionScope;
  threadId: string;
  onNewChat: () => void;
}) {
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
    // Follows the reply as it streams. Anchoring each question to the top made the scroller jump back to an older one
    // whenever the Thinking marker gave way to the reply, because the message count stayed the same.
    <MessageScrollerProvider autoScroll>
      <MessageScroller className="flex-1">
        <MessageScrollerViewport>
          <MessageScrollerContent aria-busy={chat.isLoading} className="gap-3 p-3">
            {waiting.length > 0 && (
              <MessageScrollerItem>
                <section aria-label="Waiting on you" className="flex flex-col gap-2">
                  <h3 className="text-xs font-medium text-muted-foreground">Waiting on you</h3>
                  {waiting.map((approval) => <ApprovalCard key={approval.id} organizationSlug={organizationSlug} id={approval.id} seed={approval} />)}
                </section>
              </MessageScrollerItem>
            )}
            {chat.messages.length === 0 && waiting.length === 0 && !chat.isHydrating && !chat.error && (
              <MessageScrollerItem>
                <Empty>
                  <EmptyHeader>
                    <EmptyDescription>Ask about your Projects, Deployments and Servers, or have the agent deploy for you. It stops to ask before anything is destroyed.</EmptyDescription>
                  </EmptyHeader>
                </Empty>
              </MessageScrollerItem>
            )}
            {chat.messages.map((message) => (
              <MessageScrollerItem key={message.id} messageId={message.id}>
                <Turn organizationSlug={organizationSlug} scope={scope} message={message} asked={asked} bound={bound} />
              </MessageScrollerItem>
            ))}
            {chat.status === "submitted" && (
              <MessageScrollerItem>
                <Message>
                  <Marker role="status">
                    <MarkerIcon><Spinner /></MarkerIcon>
                    <MarkerContent className="shimmer">Thinking…</MarkerContent>
                  </Marker>
                </Message>
              </MessageScrollerItem>
            )}
            {chat.error && (
              <MessageScrollerItem>
                {refused(chat.error) ? (
                  <div role="alert" className="flex items-center gap-2 text-sm text-muted-foreground">
                    This chat isn't yours.
                    <Button size="sm" variant="outline" onClick={onNewChat}>New chat</Button>
                  </div>
                ) : (
                  <Message>
                    <Bubble variant="destructive"><BubbleContent role="alert">{chat.error.message}</BubbleContent></Bubble>
                  </Message>
                )}
              </MessageScrollerItem>
            )}
          </MessageScrollerContent>
        </MessageScrollerViewport>
        <MessageScrollerButton />
      </MessageScroller>
      <Composer busy={chat.isLoading} onSend={(text) => void chat.sendMessage(text)} />
    </MessageScrollerProvider>
  );
}

/** Replies stream in, so remend closes the bold or code span still being written. A wide table scrolls on its own. */
const markdownComponents: Components = {
  table: ({ node: _node, ...props }) => <div className="typeset-scroll"><table {...props} /></div>,
  a: ({ node: _node, ...props }) => <a {...props} target="_blank" rel="noreferrer" />,
};

type Bound = { key: string; approvalId: string; interrupt: { canResolve: boolean; status: string; resolveInterrupt: (response: Record<string, never>) => void } };

/** One turn of the conversation. A call a human denied says so once, on its approval card. */
export function Turn({ organizationSlug, scope, message, asked, bound }: {
  organizationSlug: string;
  scope: CollectionScope;
  message: UIMessage;
  asked: Asked;
  bound: readonly Bound[];
}) {
  if (message.role === "user") {
    const text = message.parts.flatMap((part) => part.type === "text" ? [part.content] : []).join("\n");
    return (
      <Message align="end">
        <MessageContent>
          <Bubble variant="muted"><BubbleContent className="whitespace-pre-wrap">{text}</BubbleContent></Bubble>
        </MessageContent>
      </Message>
    );
  }
  const results = new Map(message.parts.flatMap((part) =>
    part.type === "tool-result" ? [[part.toolCallId, part] as const] : []));
  return (
    <Message>
      <MessageContent>
        {message.parts.map((part, index) => {
          if (part.type === "text") {
            if (!part.content.trim()) return null;
            return (
              <Bubble key={index} variant="ghost">
                <BubbleContent className="typeset typeset-chat">
                  <Markdown remarkPlugins={[remarkGfm]} components={markdownComponents}>{remend(part.content)}</Markdown>
                </BubbleContent>
              </Bubble>
            );
          }
          if (part.type !== "tool-call") return null;
          const waitingOn = bound.find(({ key }) => key === part.id);
          const approvalId = waitingOn?.approvalId ?? asked[part.id];
          const outcome = toolOutcome(part, results.get(part.id));
          const deniedOnCard = approvalId !== undefined
            && Option.exists(outcome, (answer) => "refusal" in answer && answer.refusal.code === "approval_denied");
          const settle = waitingOn && (() => {
            if (waitingOn.interrupt.canResolve && waitingOn.interrupt.status === "pending") waitingOn.interrupt.resolveInterrupt({});
          });
          return (
            <Fragment key={part.id}>
              {approvalId && <ApprovalCard organizationSlug={organizationSlug} id={approvalId} autoFocus={waitingOn !== undefined} onSettled={settle} />}
              {(!approvalId || Option.isSome(outcome)) && !deniedOnCard && (
                <ToolRow organizationSlug={organizationSlug} scope={scope} name={part.name} outcome={outcome}
                  done={part.state === "complete" || results.has(part.id)} />
              )}
            </Fragment>
          );
        })}
      </MessageContent>
    </Message>
  );
}

function Composer({ busy, onSend }: { busy: boolean; onSend: (text: string) => void }) {
  const [draft, setDraft] = useState("");
  const { scrollToEnd } = useMessageScroller();
  const send = (event?: FormEvent) => {
    event?.preventDefault();
    const text = draft.trim();
    if (!text || busy) return;
    onSend(text);
    setDraft("");
    // Sending from further up the chat brings the new turn into view, and following resumes from there.
    scrollToEnd();
  };
  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && !event.shiftKey && !event.nativeEvent.isComposing) send(event);
  };
  return (
    <form onSubmit={send} className="shrink-0 border-t p-2">
      <InputGroup>
        <InputGroupTextarea aria-label="Message the agent" placeholder="Ask about your servers…" rows={1} value={draft}
          onChange={(event) => setDraft(event.target.value)} onKeyDown={onKeyDown} className="max-h-40 min-h-9" />
        <InputGroupAddon align="block-end">
          <InputGroupButton type="submit" size="icon-sm" variant="default" className="ml-auto" disabled={busy || !draft.trim()}>
            <ArrowUpIcon />
            <span className="sr-only">Send</span>
          </InputGroupButton>
        </InputGroupAddon>
      </InputGroup>
    </form>
  );
}
