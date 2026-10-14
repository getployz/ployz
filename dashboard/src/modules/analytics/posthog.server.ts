import "@tanstack/react-start/server-only";
import { PostHogMCP } from "@posthog/mcp";
import { Context, Effect, Layer } from "effect";
import { AppConfig } from "#/server/config.server";

type Properties = Record<string, string | number | boolean | null>;

export type PostHogService = {
  /** What a user did, credited to their Organization when it happened in one. */
  readonly capture: (input: {
    readonly userId: string;
    readonly event: string;
    readonly organizationId?: string;
    readonly properties?: Properties;
  }) => Effect.Effect<void>;
  readonly identify: (userId: string, properties: Properties) => Effect.Effect<void>;
  readonly identifyOrganization: (organizationId: string, properties: Properties) => Effect.Effect<void>;
  /** A sidebar agent tool call, as MCP Analytics' `$mcp_tool_call`, without its arguments or result. */
  readonly toolCall: (input: {
    readonly userId: string;
    readonly organizationId: string;
    readonly threadId: string;
    readonly tool: string;
    readonly durationMs: number;
    readonly failed: boolean;
  }) => Effect.Effect<void>;
};

const disabled: PostHogService = {
  capture: () => Effect.void,
  identify: () => Effect.void,
  identifyOrganization: () => Effect.void,
  toolCall: () => Effect.void,
};

/**
 * Product analytics on Ployz-hosted Cloud; a no-op on a Self-hosted Cloud and wherever no layer provides it (tests).
 * Analytics never fails the work it describes: the SDK only queues, and anything it throws is logged.
 */
export const PostHog = Context.Reference<PostHogService>("ployz/PostHog", {
  defaultValue: () => disabled,
});

const bestEffort = (operation: string, run: () => void) =>
  Effect.try({ try: run, catch: (cause) => cause }).pipe(
    Effect.catch((cause) => Effect.logWarning(`PostHog ${operation} was dropped.`, cause)),
  );

export const PostHogLive = Layer.effect(
  PostHog,
  Effect.gen(function* () {
    const { posthog } = yield* AppConfig;
    if (posthog === null) return disabled;
    const client = yield* Effect.acquireRelease(
      // An agent's failed tool call is no exception: the call's own `$mcp_is_error` says it failed.
      Effect.sync(() => new PostHogMCP(posthog.key, { host: posthog.host, enableExceptionAutocapture: false })),
      // Shutdown flushes what is still queued.
      (client) => Effect.tryPromise(() => client.shutdown()).pipe(
        Effect.catch((cause) => Effect.logWarning("PostHog could not flush its queue on shutdown.", cause)),
      ),
    );
    return {
      capture: ({ userId, event, organizationId, properties }) =>
        bestEffort("capture", () => client.capture({
          distinctId: userId,
          event,
          properties,
          groups: organizationId === undefined ? undefined : { organization: organizationId },
        })),
      identify: (userId, properties) =>
        bestEffort("identify", () => client.identify({ distinctId: userId, properties: { $set: properties } })),
      identifyOrganization: (organizationId, properties) =>
        bestEffort("groupIdentify", () => client.groupIdentify({ groupType: "organization", groupKey: organizationId, properties })),
      toolCall: ({ userId, organizationId, threadId, tool, durationMs, failed }) =>
        bestEffort("captureToolCall", () => client.captureToolCall({
          distinctId: userId,
          groups: { organization: organizationId },
          sessionId: threadId,
          toolName: tool,
          durationMs,
          isError: failed,
          properties: { $mcp_server_name: "ployz-sidebar" },
        })),
    } satisfies PostHogService;
  }),
);

/** What the browser needs to start PostHog; null when Cloud runs without it. */
export const postHogBrowserConfig = Effect.map(AppConfig, (config) => config.posthog);

export const recordSignUp = Effect.fn("PostHog.recordSignUp")(function* (user: {
  readonly id: string;
  readonly email: string;
  readonly name: string;
}) {
  const posthog = yield* PostHog;
  yield* posthog.identify(user.id, { email: user.email, name: user.name });
  yield* posthog.capture({ userId: user.id, event: "user_signed_up" });
});
