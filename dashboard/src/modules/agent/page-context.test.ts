import type { ModelMessage } from "@tanstack/ai";
import { Option, Schema } from "effect";
import { describe, expect, it } from "vitest";
import { isPageContext, PageContext, latestPageContext, renderPageContext, withPageContext } from "./page-context";

describe("renderPageContext", () => {
  it("writes every named field in a fixed order, whatever order the object holds them in", () => {
    expect(renderPageContext({ service: "api", environment: "production", server: "srv-1", page: "architecture", deployment: "dep-1", project: "web" }))
      .toBe('<dashboard-page page="architecture" project="web" environment="production" service="api" deployment="dep-1" server="srv-1"/>');
  });

  it("leaves out what the page does not name", () => {
    expect(renderPageContext({ page: "servers" })).toBe('<dashboard-page page="servers"/>');
  });

  it("escapes values so none can close the attribute or the tag", () => {
    expect(renderPageContext({ page: "a&b", service: '"/><x y="' }))
      .toBe('<dashboard-page page="a&amp;b" service="&quot;/&gt;&lt;x y=&quot;"/>');
  });
});

const decodePageContext = (forwarded: PageContext | string | undefined | Readonly<Record<string, string | number>>) =>
  Option.getOrUndefined(Schema.decodeUnknownOption(PageContext)(forwarded));

describe("PageContext", () => {
  it("keeps a well-formed page", () => {
    expect(decodePageContext({ page: "architecture", project: "web" })).toEqual({ page: "architecture", project: "web" });
  });

  it.each([
    ["nothing", undefined],
    ["a string", "architecture"],
    ["no page", { project: "web" }],
    ["an empty page", { page: "" }],
    ["a number", { page: "architecture", service: 7 }],
    ["an overlong value", { page: "architecture", project: "x".repeat(201) }],
  ])("treats %s as no page", (_name, forwarded) => {
    expect(decodePageContext(forwarded)).toBeUndefined();
  });
});

const block = (page: string) => renderPageContext({ page });
const said = (...content: string[]): ModelMessage => ({ role: "user", content: content.map((text) => ({ type: "text", content: text })) });

describe("latestPageContext", () => {
  it("is the newest member message's opening block", () => {
    expect(latestPageContext([
      said(block("architecture"), "hi"),
      { role: "assistant", content: "hello" },
      said(block("servers"), "and now"),
      said("still here"),
    ])).toBe(block("servers"));
  });

  it("ignores a block that does not open the message and anything the agent wrote", () => {
    expect(latestPageContext([said("hi", block("servers")), { role: "assistant", content: block("logs") }])).toBeUndefined();
  });

  it("is nothing for a thread without one", () => {
    expect(latestPageContext([said("hi")])).toBeUndefined();
    expect(isPageContext("hi")).toBe(false);
  });
});

describe("withPageContext", () => {
  it("opens the message with the block, replacing any block it already had", () => {
    expect(withPageContext(said(block("logs"), "hi"), block("servers"))).toEqual(said(block("servers"), "hi"));
    expect(withPageContext({ role: "user", content: "hi" }, block("servers"))).toEqual(said(block("servers"), "hi"));
  });
});
