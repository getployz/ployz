import type { ModelMessage } from "@tanstack/ai";
import { Effect, Option, Schema } from "effect";
import { describe, expect, it } from "vitest";
import { isPageContext, PageContext, latestPageContext, placeOfPage, renderPageContext, withPageContext } from "./page-context";

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

describe("placeOfPage", () => {
  type Store = Readonly<Record<string, ReadonlyArray<{ id: string; name: string }>>>;
  const place = (store: Store, context: PageContext) => Effect.runSync(placeOfPage(
    renderPageContext(context),
    Effect.sync(() => Object.keys(store)),
    (project) => Effect.sync(() => ({ project, environments: store[project] ?? [] })),
  ));
  const shop = { page: "services", project: "shop", environment: "production", environment_id: "env-1" };

  it("names a page's Environment and Project by their current names after the URL went stale", () => {
    expect(place({ store: [{ id: "env-1", name: "production" }] }, shop)).toEqual({ project: "store", environment: "production" });
  });

  it("names a new Project that took a renamed one's old name, not the renamed one", () => {
    const store = { store: [{ id: "env-1", name: "production" }], docs: [{ id: "env-2", name: "production" }], shop: [{ id: "env-3", name: "production" }] };
    expect(place(store, { ...shop, environment_id: "env-3" })).toEqual({ project: "shop", environment: "production" });
    expect(place(store, shop)).toEqual({ project: "store", environment: "production" });
  });

  it("follows an Environment's rename", () => {
    expect(place({ shop: [{ id: "env-1", name: "live" }] }, shop)).toEqual({ project: "shop", environment: "live" });
  });

  it("keeps the page's names when it carries no Environment id or no Environment has it", () => {
    expect(place({ store: [{ id: "env-1", name: "production" }] }, { page: "services", project: "shop", environment: "production" }))
      .toEqual({ project: "shop", environment: "production" });
    expect(place({}, shop)).toEqual({ project: "shop", environment: "production" });
    expect(place({}, { page: "servers" })).toEqual({ project: null, environment: null });
  });
});
