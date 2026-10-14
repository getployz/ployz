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

export type Listing = { project: string; environments: ReadonlyArray<{ id: string; name: string }> };

/**
 * Where the page puts the agent. The dashboard keeps a page's old names after a rename, and a new Project can take an
 * old name, so the page's Environment id decides and its names are only where to look first.
 */
export type PagePlace =
  /** No page, or one outside any Environment: a tool takes the Organization's defaults. */
  | { readonly at: "organization" }
  /** The page's Environment, by its current names. */
  | { readonly at: "here"; readonly project: string; readonly environment: string }
  /** No Environment has the page's id any more; the names are what the page showed. */
  | { readonly at: "gone"; readonly project: string | null; readonly environment: string | null }
  /** The Store did not answer where the page's Environment is. */
  | { readonly at: "unread" };

const ORGANIZATION: PagePlace = { at: "organization" };

/** Where `block` puts the agent, by current names. */
export const placeOfPage = <E, R>(
  block: string | undefined,
  projects: Effect.Effect<ReadonlyArray<string>, E, R>,
  environmentsOf: (project: string) => Effect.Effect<Listing, E, R>,
): Effect.Effect<PagePlace, never, R> => Effect.gen(function* () {
  const at = block === undefined ? new Map<string, string>() : readPageContext(block);
  const project = at.get("project");
  const environment = at.get("environment");
  const id = at.get("environment_id");
  if (id === undefined) {
    return project === undefined || environment === undefined ? ORGANIZATION : { at: "here", project, environment } as const;
  }
  const listed = yield* projects;
  const order = [...listed.filter((name) => name === project), ...listed.filter((name) => name !== project)];
  for (const name of order) {
    const listing = yield* environmentsOf(name);
    const found = listing.environments.find((candidate) => candidate.id === id);
    if (found !== undefined) return { at: "here", project: listing.project, environment: found.name } as const;
  }
  return { at: "gone", project: project ?? null, environment: environment ?? null } as const;
}).pipe(Effect.catch(() => Effect.succeed({ at: "unread" } as const)));

/** The Project and Environment a tool call runs in, or why it may not run. */
export type CallPlace =
  | { readonly ok: true; readonly project: string | null; readonly environment: string | null }
  | { readonly ok: false; readonly code: "not_found" | "unavailable"; readonly message: string };

export function placeForCall(page: PagePlace, call: { readonly scoped: boolean; readonly namesProject: boolean }): CallPlace {
  if (page.at === "here") return { ok: true, project: page.project, environment: page.environment };
  if (page.at === "organization" || !call.scoped || call.namesProject) return { ok: true, project: null, environment: null };
  if (page.at === "unread") {
    return { ok: false, code: "unavailable", message: "Ployz could not read which environment this page shows. Try again, or name the project and environment to act in." };
  }
  const shown = page.project === null || page.environment === null ? "" : ` ${page.project}/${page.environment}`;
  return { ok: false, code: "not_found", message: `The environment${shown} this page showed no longer exists. Name the project and environment to act in.` };
}
