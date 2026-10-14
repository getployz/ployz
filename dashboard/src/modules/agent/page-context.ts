import type { ModelMessage } from "@tanstack/ai";
import { Effect, Schema } from "effect";

const Value = Schema.String.check(Schema.isMinLength(1), Schema.isMaxLength(200));

/** Where in the dashboard the member is: the page they view and the Project, Environment, Service or record it names. */
export const PageContext = Schema.Struct({
  page: Value,
  project: Schema.optional(Value),
  environment: Schema.optional(Value),
  /** The Environment's id, which outlives renames of it and its Project. */
  environment_id: Schema.optional(Value),
  /** The Service's name, the way tools take it. */
  service: Schema.optional(Value),
  deployment: Schema.optional(Value),
  server: Schema.optional(Value),
});
export type PageContext = typeof PageContext.Type;

const TAG = "<dashboard-page ";
const ORDER = ["page", "project", "environment", "environment_id", "service", "deployment", "server"] as const satisfies ReadonlyArray<keyof PageContext>;

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

const unescape = (value: string) =>
  value.replaceAll("&quot;", "\"").replaceAll("&lt;", "<").replaceAll("&gt;", ">").replaceAll("&amp;", "&");

export function readPageContext(block: string): ReadonlyMap<string, string> {
  return new Map([...block.matchAll(/(\w+)="([^"]*)"/g)].map(([, key = "", value = ""]) => [key, unescape(value)]));
}

/** `message` opening with `block`, its own text after it. */
export function withPageContext(message: ModelMessage, block: string): ModelMessage {
  const own = Array.isArray(message.content)
    ? message.content.filter((part) => !(part.type === "text" && isPageContext(part.content)))
    : message.content ? [{ type: "text" as const, content: message.content }] : [];
  return { ...message, content: [{ type: "text", content: block }, ...own] };
}

/** A Project's Environments, by id and current name. */
export type Listing = { project: string; environments: ReadonlyArray<{ id: string; name: string }> };

/**
 * The Project and Environment `block` names, by their current names. The dashboard keeps a page's old names after a
 * rename, and a new Project can take an old name, so a block's Environment id decides; its names are only where to look
 * first. An id no Environment has names no place, since the old names may now belong to another Project.
 */
export const placeOfPage = <E, R>(
  block: string | undefined,
  projects: Effect.Effect<ReadonlyArray<string>, E, R>,
  environmentsOf: (project: string) => Effect.Effect<Listing, E, R>,
) => Effect.gen(function* () {
  const at = block === undefined ? new Map<string, string>() : readPageContext(block);
  const named = { project: at.get("project") ?? null, environment: at.get("environment") ?? null };
  const id = at.get("environment_id");
  if (id === undefined) return named;
  const listed = yield* projects;
  const order = [...listed.filter((project) => project === named.project), ...listed.filter((project) => project !== named.project)];
  for (const project of order) {
    const listing = yield* environmentsOf(project);
    const environment = listing.environments.find((candidate) => candidate.id === id);
    if (environment !== undefined) return { project: listing.project, environment: environment.name };
  }
  return { project: null, environment: null };
});
