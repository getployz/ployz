import type { ModelMessage } from "@tanstack/ai";
import { Schema } from "effect";

const Value = Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(200));

/** Where in the dashboard the member is: the page they view and the Project, Environment, Service or record it names. */
export const PageContext = Schema.Struct({
  page: Value,
  project: Schema.optional(Value),
  environment: Schema.optional(Value),
  /** The Service's name, the way tools take it. */
  service: Schema.optional(Value),
  deployment: Schema.optional(Value),
  server: Schema.optional(Value),
});
export type PageContext = typeof PageContext.Type;

const TAG = "<dashboard-page ";
const ORDER = ["page", "project", "environment", "service", "deployment", "server"] as const satisfies ReadonlyArray<keyof PageContext>;

const escape = (value: string) =>
  value.replaceAll("&", "&amp;").replaceAll("\"", "&quot;").replaceAll("<", "&lt;").replaceAll(">", "&gt;");

/** One line, the same bytes for the same place, so a block repeats exactly and the prompt prefix stays cacheable. */
export function renderPageContext(context: PageContext): string {
  const attributes = ORDER.flatMap((key) => {
    const value = context[key];
    return value === undefined ? [] : [`${key}="${escape(value)}"`];
  });
  return `${TAG}${attributes.join(" ")}/>`;
}

export const isPageContext = (text: string) => text.startsWith(TAG);

/** The block a member message opens with, if any. */
export function pageContextOf(message: ModelMessage): string | undefined {
  if (message.role !== "user" || !Array.isArray(message.content)) return undefined;
  const [first] = message.content;
  return first?.type === "text" && isPageContext(first.content) ? first.content : undefined;
}

/** The newest block in a stored thread: where the member last was. */
export function latestPageContext(messages: ReadonlyArray<ModelMessage>): string | undefined {
  for (let index = messages.length - 1; index >= 0; index--) {
    const message = messages[index];
    const block = message === undefined ? undefined : pageContextOf(message);
    if (block !== undefined) return block;
  }
  return undefined;
}

/** `message` opening with `block`, its own text after it. */
export function withPageContext(message: ModelMessage, block: string): ModelMessage {
  const own = Array.isArray(message.content)
    ? message.content.filter((part) => !(part.type === "text" && isPageContext(part.content)))
    : message.content ? [{ type: "text" as const, content: message.content }] : [];
  return { ...message, content: [{ type: "text", content: block }, ...own] };
}
