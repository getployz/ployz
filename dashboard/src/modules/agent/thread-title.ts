import type { ModelMessage } from "@tanstack/ai";
import { isPageContext } from "#/modules/agent/page-context";

const MAX_LENGTH = 80;

/** A chat's title: the member's own words in its first message, on one line. None when they wrote nothing. */
export function threadTitle(message: ModelMessage | null | undefined): string | undefined {
  if (message?.role !== "user") return undefined;
  const parts = Array.isArray(message.content)
    ? message.content.flatMap((part) => part.type === "text" && !isPageContext(part.content) ? [part.content] : [])
    : [message.content ?? ""];
  return chatTitle(parts.join(" "));
}

/** `words` on one line, cut to a title's length. None when there are no words. */
export function chatTitle(words: string): string | undefined {
  const text = words.replace(/\s+/g, " ").trim();
  if (text === "") return undefined;
  return text.length <= MAX_LENGTH ? text : `${text.slice(0, MAX_LENGTH - 1).trimEnd()}…`;
}
