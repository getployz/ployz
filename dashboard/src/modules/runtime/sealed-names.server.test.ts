import { Effect, Exit, Scope } from "effect";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ConfigStore, SealedNames } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import { sealedFeed } from "./sealed-names.server";

type Landed = { names?: readonly string[] | null; failed?: true };

/** Stands in for the native handle: records each read as it lands, and plays a reader asking for one. */
class FakeNames {
  reads: Landed[] = [];
  // Like the native handle: an ask before anyone waits is kept, and an ask wakes the oldest waiter only.
  asked = false;
  wakes: (() => void)[] = [];
  want() { const wake = this.wakes.shift(); if (wake) wake(); else this.asked = true; }
  begin() {
    const read: Landed = {};
    this.reads.push(read);
    return { done: (names: readonly string[] | null) => { read.names = names; }, failed: () => { read.failed = true; } };
  }
  wanted() {
    if (this.asked) { this.asked = false; return Promise.resolve(); }
    return new Promise<void>(resolve => { this.wakes.push(resolve); });
  }
}

beforeEach(() => { vi.useFakeTimers(); });
afterEach(() => { vi.useRealTimers(); });

/** A Store owning Namespace `app` in `web/production`, answering its Sealed names with `answer`. */
async function feed(answer: () => readonly string[]) {
  const scope = Effect.runSync(Scope.make());
  const store = asTestDouble<ConfigStore>()({
    read: async (_organizationId: string, query: { query: string }) => query.query === "namespaces"
      ? { namespaces: [{ namespace: "app", project: "web", environment: "production" }] }
      : { names: answer() },
  });
  const handle = new FakeNames();
  await Effect.runPromise(sealedFeed("org", "app", asTestDouble<SealedNames>()(handle)).pipe(
    Effect.provideService(Scope.Scope, scope),
    Effect.provideService(CloudStore, { open: Effect.succeed(store) }),
  ));
  return { handle, close: () => Effect.runPromise(Scope.close(scope, Exit.void)) };
}

it("rereads the Store every five seconds, and at once when a reader asks", async () => {
  let names = ["TOKEN"];
  const { handle, close } = await feed(() => names);
  try {
    await vi.advanceTimersByTimeAsync(4_999);
    expect(handle.reads).toEqual([]);
    await vi.advanceTimersByTimeAsync(1);
    expect(handle.reads).toEqual([{ names: ["TOKEN"] }]);
    names = ["TOKEN", "KEY"];
    await vi.advanceTimersByTimeAsync(1_000);
    handle.want();
    await vi.advanceTimersByTimeAsync(0);
    expect(handle.reads).toEqual([{ names: ["TOKEN"] }, { names: ["TOKEN", "KEY"] }]);
  } finally { await close(); }
});

it("fails the read when the Store can't answer, and keeps reading", async () => {
  let broken = true;
  const { handle, close } = await feed(() => { if (broken) throw new Error("the Store is down"); return []; });
  try {
    handle.want();
    await vi.advanceTimersByTimeAsync(0);
    expect(handle.reads).toEqual([{ failed: true }]);
    broken = false;
    // The next wait starts a scheduler tick after the read lands.
    await vi.advanceTimersByTimeAsync(4_990);
    expect(handle.reads).toEqual([{ failed: true }]);
    await vi.advanceTimersByTimeAsync(20);
    expect(handle.reads).toEqual([{ failed: true }, { names: [] }]);
  } finally { await close(); }
});
