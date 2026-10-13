import { describe, expect, it } from "vitest";
import { renderPageContext } from "./page-context";
import { threadTitle } from "./thread-title";

const block = renderPageContext({ page: "architecture", project: "shop" });

describe("threadTitle", () => {
  it("is the member's words after the page block, on one line", () => {
    expect(threadTitle({ role: "user", content: [{ type: "text", content: block }, { type: "text", content: "  clean up\n pg-data-old " }] }))
      .toBe("clean up pg-data-old");
  });

  it("takes a plain string message as it is", () => {
    expect(threadTitle({ role: "user", content: "why is api restarting?" })).toBe("why is api restarting?");
  });

  it("is none when the member wrote nothing of their own, or the message isn't theirs", () => {
    expect(threadTitle({ role: "user", content: [{ type: "text", content: block }] })).toBeUndefined();
    expect(threadTitle({ role: "user", content: "   " })).toBeUndefined();
    expect(threadTitle({ role: "assistant", content: "hello" })).toBeUndefined();
    expect(threadTitle(null)).toBeUndefined();
  });

  it("cuts a long message to 80 characters with an ellipsis", () => {
    const title = threadTitle({ role: "user", content: "word ".repeat(40) });
    expect(title).toHaveLength(80);
    expect(title?.endsWith("word…") || title?.endsWith("wor…")).toBe(true);
  });
});
