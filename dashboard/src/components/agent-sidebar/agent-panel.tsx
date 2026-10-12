import { Fragment, useEffect, useMemo, useState, type FormEvent, type KeyboardEvent, type ReactNode } from "react";
import { useQuery } from "@tanstack/react-query";
import { EventType, type StreamChunk } from "@tanstack/ai/client";
import { fetchServerSentEvents, useChat, type UIMessage } from "@tanstack/ai-react";
import { Option, Schema } from "effect";
import Markdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import remend from "remend";
import { ArrowUpIcon, ChevronRightIcon, SquareIcon, SquarePenIcon, XIcon } from "lucide-react";
import type { CollectionScope } from "#/collections/scope";
import { Bubble, BubbleContent } from "#/components/ui/bubble";
import { Button } from "#/components/ui/button";
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from "#/components/ui/collapsible";
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
import { isPageContext, type PageContext } from "#/modules/agent/page-context";
import type { Failure } from "#/modules/agent/provider-error";
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
  /** Where the member is; each message carries it, so the agent reads "this service" as the one on screen. */
  page: PageContext;
  threadId: string;
  onNewChat: () => void;
  onClose: () => void;
};

/** One agent conversation; the sidebar picks the thread, and switching Organization or thread swaps in a fresh connection. */
export default function AgentPanel({ organizationSlug, environment, scope, page, threadId, onNewChat, onClose }: PanelProps) {
  return (
    <section aria-label="Ployz agent" className="flex h-full min-h-0 flex-col bg-background">
      <header className="flex h-12 shrink-0 items-center gap-2 border-b px-3">
        <h2 className="text-sm font-semibold">Ployz agent</h2>
        <span className="truncate text-xs text-muted-foreground">{environment ? `${organizationSlug} / ${environment}` : organizationSlug}</span>
        <Button className="ml-auto" size="icon-sm" variant="ghost" aria-label="New chat" title="New chat" onClick={onNewChat}><SquarePenIcon /></Button>
        <Button size="icon-sm" variant="ghost" aria-label="Close agent" title="Close" onClick={onClose}><XIcon /></Button>
      </header>
      <Conversation key={`${organizationSlug}/${threadId}`} organizationSlug={organizationSlug} scope={scope} page={page} threadId={threadId} onNewChat={onNewChat} />
    </section>
  );
}

/** The client reports any refused request only as text; a 403 means the thread in the link is another member's. */
const refused = (error: Error | undefined) => error?.message.includes("status: 403") ?? false;

function Conversation({ organizationSlug, scope, page, threadId, onNewChat }: {
  organizationSlug: string;
  scope: CollectionScope;
  page: PageContext;
  threadId: string;
  onNewChat: () => void;
}) {
  const [connection] = useState(() => fetchServerSentEvents(`/api/agent/${encodeURIComponent(organizationSlug)}/chat`));
  // useChat hands new forwardedProps to its client on every change of identity.
  const forwardedProps = useMemo(() => ({ page }), [page]);
  const [retryAt, setRetryAt] = useState<number | null>(null);
  const [tooLong, setTooLong] = useState(false);
  const onChunk = (chunk: StreamChunk) => {
    if (chunk.type !== EventType.RUN_ERROR) return;
    const wait = chunk.metadata?.tanstack?.retryAfterMs;
    setRetryAt(wait === undefined ? null : Date.now() + wait);
    setTooLong(chunk.code === ("chat_too_long" satisfies Failure));
  };
  const chat = useChat({ connection, threadId, persistence: true, interrupts: [approvalInterrupt], live: true, forwardedProps, onChunk });
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
            {chat.isLoading && !replying(chat.messages) && (
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
                  <Failed message={chat.error.message}>
                    {tooLong
                      ? <Button size="sm" variant="outline" className="self-start" onClick={onNewChat}>New chat</Button>
                      : <RetryButton retryAt={retryAt} onRetry={() => {
                        setRetryAt(null);
                        void chat.reload();
                      }} />}
                  </Failed>
                )}
              </MessageScrollerItem>
            )}
          </MessageScrollerContent>
        </MessageScrollerViewport>
        <MessageScrollerButton />
      </MessageScroller>
      <Composer busy={chat.isLoading} onSend={(text) => void chat.sendMessage(text)} onStop={chat.stop} />
    </MessageScrollerProvider>
  );
}

/** Seconds left until `at`, ticking down to 0. */
function useSecondsUntil(at: number | null) {
  const [now, setNow] = useState(Date.now);
  const left = at === null ? 0 : Math.ceil((at - now) / 1000);
  useEffect(() => {
    if (at === null || left <= 0) return;
    const tick = setTimeout(() => setNow(Date.now()), at - now - (left - 1) * 1000);
    return () => clearTimeout(tick);
  }, [at, now, left]);
  return Math.max(0, left);
}

/** A reply that failed, and what the member can do about it. */
function Failed({ message, children }: { message: string; children: ReactNode }) {
  return (
    <Message>
      <MessageContent>
        <Bubble variant="destructive"><BubbleContent role="alert">{message}</BubbleContent></Bubble>
        {children}
      </MessageContent>
    </Message>
  );
}

/** Retry waits as long as a rate-limited model asked. */
function RetryButton({ retryAt, onRetry }: { retryAt: number | null; onRetry: () => void }) {
  const wait = useSecondsUntil(retryAt);
  return <Button size="sm" variant="outline" className="self-start" disabled={wait > 0} onClick={onRetry}>{wait > 0 ? `Retry in ${wait}s` : "Retry"}</Button>;
}

/** Replies stream in, so remend closes the bold or code span still being written. A wide table scrolls on its own. */
const markdownComponents: Components = {
  table: ({ node: _node, ...props }) => <div className="typeset-scroll"><table {...props} /></div>,
  a: ({ node: _node, ...props }) => <a {...props} target="_blank" rel="noreferrer" />,
};

type Part = UIMessage["parts"][number];

const shows = (part: Part) =>
  part.type === "tool-call" || ((part.type === "text" || part.type === "thinking") && part.content.trim() !== "");

/** Whether the reply to the latest message shows anything yet; until it does, the Thinking marker stands in for it. */
const replying = (messages: readonly UIMessage[]) =>
  messages.slice(messages.map(({ role }) => role).lastIndexOf("user") + 1).some(({ parts }) => parts.some(shows));

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
    const text = message.parts.flatMap((part) => part.type === "text" && !isPageContext(part.content) ? [part.content] : []).join("\n");
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
          if (!shows(part)) return null;
          if (part.type === "thinking") {
            return (
              <Collapsible key={index}>
                <CollapsibleTrigger render={<Button type="button" variant="ghost" size="sm" className="group self-start text-muted-foreground" />}>
                  Thinking
                  <ChevronRightIcon data-icon="inline-end" className="transition-transform group-data-[panel-open]:rotate-90" />
                </CollapsibleTrigger>
                <CollapsibleContent className="whitespace-pre-wrap px-2 text-sm text-muted-foreground">{part.content}</CollapsibleContent>
              </Collapsible>
            );
          }
          if (part.type === "text") {
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

function Composer({ busy, onSend, onStop }: { busy: boolean; onSend: (text: string) => void; onStop: () => void }) {
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
          {busy ? (
            <InputGroupButton type="button" size="icon-sm" variant="default" className="ml-auto" onClick={onStop}>
              <SquareIcon className="fill-current" />
              <span className="sr-only">Stop</span>
            </InputGroupButton>
          ) : (
            <InputGroupButton type="submit" size="icon-sm" variant="default" className="ml-auto" disabled={!draft.trim()}>
              <ArrowUpIcon />
              <span className="sr-only">Send</span>
            </InputGroupButton>
          )}
        </InputGroupAddon>
      </InputGroup>
    </form>
  );
}
