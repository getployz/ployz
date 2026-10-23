import type { ConfigCommand } from "@ployz/sdk";
import { Effect } from "effect";
import { expect, it } from "vitest";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { type AgentCaller, runGated } from "./agent-runner.server";

const caller: AgentCaller = {
  userId: "user", organization: { id: "org", slug: "shop" }, credential: { kind: "session", id: "session" }, role: "owner", support: null, via: "agent",
};
const production = { project: "shop", environment: "production" };
const sealed = { sealedFor: "00000000-0000-4000-8000-00000000a704", value: { sealed: "AAAA", scope: { service: "00000000-0000-4000-8000-00000000a705" }, key: "TOKEN" } };

it.each<[string, ConfigCommand]>([
  ["a browser-sealed value", { command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "web.env.TOKEN", value: { secret: sealed } }] }],
  ["a secret's value as text", { command: "edit", environment: production, expect: null, changes: [{ op: "set", path: "web.env.TOKEN", value: { secret: "tok_live_123456" } }] }],
  ["a sealed value in a Sync", { command: "sync", from: { project: "shop", environment: "fix-api" }, into: production, picks: [], values: { "a:variables.TOKEN": sealed }, version: "1", id: "00000000-0000-4000-8000-00000000a706", close_after: false }],
])("refuses a gated Store write carrying %s before any approval or Store call", async (_, asked) => {
  // Every Store call opens the Store first.
  let opened = 0;
  const store = { open: Effect.suspend(() => { opened += 1; return Effect.die("reached the Store"); }) };
  const gated = await Effect.runPromise(runGated(caller, asked, null).pipe(Effect.provideService(CloudStore, store)) as Effect.Effect<unknown>);
  expect(gated).toMatchObject({ result: { ok: false, refusal: { code: "invalid_argument" } }, nothingDestroyed: false });
  expect(opened).toBe(0);
});
