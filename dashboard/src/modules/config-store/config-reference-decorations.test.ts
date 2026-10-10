import { describe, expect, it } from "vitest";
import { configReferences, previewSegments, type ReferenceValue } from "./config-references";

const values = new Map<string, ReferenceValue>([
  ["redis.PLOYZ_PRIVATE_DOMAIN", { secret: false, value: "redis.internal" }],
  ["redis.PORT", { secret: false, value: "6379" }],
  ["web.SENTRY_DSN", { secret: true }],
]);
const services = ["redis", "web"];

const spans = (text: string) => configReferences(text, values, services)
  .map((reference) => ({ ...reference, text: text.slice(reference.from, reference.to) }));

describe("configReferences", () => {
  it("covers each known reference exactly, with its secrecy", () => {
    const text = "host: ${{ redis.PLOYZ_PRIVATE_DOMAIN }}:${{redis.PORT}}\ndsn: ${{ web.SENTRY_DSN }}\n";
    expect(spans(text)).toEqual([
      { from: 6, to: 39, kind: "ref", service: "redis", key: "PLOYZ_PRIVATE_DOMAIN", secret: false, text: "${{ redis.PLOYZ_PRIVATE_DOMAIN }}" },
      { from: 40, to: 55, kind: "ref", service: "redis", key: "PORT", secret: false, text: "${{redis.PORT}}" },
      { from: 61, to: 82, kind: "ref", service: "web", key: "SENTRY_DSN", secret: true, text: "${{ web.SENTRY_DSN }}" },
    ]);
  });

  it("flags an unknown service over the whole token", () => {
    expect(spans("port: ${{ snuba.PORT }}")).toEqual([
      { from: 6, to: 23, kind: "unknown", message: "Unknown service snuba", text: "${{ snuba.PORT }}" },
    ]);
  });

  it("suggests a close service name", () => {
    expect(spans("${{ redsi.PORT }}").map((reference) => reference.kind === "unknown" && reference.message))
      .toEqual(["Unknown service redsi · Did you mean redis?"]);
  });

  it("flags a missing key, suggesting a close one", () => {
    expect(spans("${{ redis.FOO }} ${{ redis.PROT }}")).toEqual([
      { from: 0, to: 16, kind: "unknown", message: "redis has no FOO", text: "${{ redis.FOO }}" },
      { from: 17, to: 34, kind: "unknown", message: "redis has no PROT · Did you mean PORT?", text: "${{ redis.PROT }}" },
    ]);
  });

  it("refuses a bare key, since Configs are shared", () => {
    expect(spans("${{ PORT }}")).toEqual([
      { from: 0, to: 11, kind: "unknown", message: "Configs are shared. Write ${{ service.KEY }}.", text: "${{ PORT }}" },
    ]);
  });

  it("leaves the $${{ escape plain", () => {
    expect(spans("literal: $${{ redis.PORT }} and $${{ snuba.X }}")).toEqual([]);
  });

  it("flags nothing until the token is closed", () => {
    expect(spans("${{ red")).toEqual([]);
    expect(spans("${{ snuba.PORT")).toEqual([]);
    expect(spans("${{ snuba.PORT }")).toEqual([]);
  });
});

describe("previewSegments", () => {
  const values = new Map<string, ReferenceValue>([
    ["redis.PLOYZ_PRIVATE_DOMAIN", { secret: false, value: "redis.internal" }],
    ["web.SENTRY_DSN", { secret: true }],
  ]);

  it("resolves values, masks secrets and keeps unknown tokens as written", () => {
    expect(previewSegments("host: ${{ redis.PLOYZ_PRIVATE_DOMAIN }}\ndsn: ${{ web.SENTRY_DSN }}\nx: ${{ snuba.PORT }}", values))
      .toEqual([
        { kind: "text", text: "host: " },
        { kind: "value", text: "redis.internal" },
        { kind: "text", text: "\ndsn: " },
        { kind: "secret" },
        { kind: "text", text: "\nx: " },
        { kind: "unknown", text: "${{ snuba.PORT }}" },
      ]);
  });

  it("never carries a secret's plaintext, even when a caller has it", () => {
    const leaky = new Map<string, ReferenceValue>([["web.SENTRY_DSN", { secret: true, value: "hunter2" } as ReferenceValue]]);
    const segments = previewSegments("dsn: ${{ web.SENTRY_DSN }}", leaky);
    expect(segments).toEqual([{ kind: "text", text: "dsn: " }, { kind: "secret" }]);
    expect(JSON.stringify(segments)).not.toContain("hunter2");
  });

  it("renders the $${{ escape as a literal ${{ in the surrounding text", () => {
    expect(previewSegments("a $${{ redis.PLOYZ_PRIVATE_DOMAIN }} b", values))
      .toEqual([{ kind: "text", text: "a ${{ redis.PLOYZ_PRIVATE_DOMAIN }} b" }]);
  });
});
