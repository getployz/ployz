import { expect, it, vi } from "vitest";
import { Effect, Option, Schema } from "effect";
import type { ConfigStore, LogHistoryOptions, LogMetadata, SealedNames } from "@ployz/sdk";
import { asTestDouble } from "#/lib/test-double";
import { CloudStore } from "#/modules/config-store/store-sdk.server";
import type { ConnectedRuntimeClient } from "./organization-runtime.server";
import { logHistorySchema, readContainerLogs } from "./container-logs.server";

it("accepts a history cursor holding a place on each of 50 Servers", () => {
  const place = { c: `1760000000000000000:${"a".repeat(64)}:12:34`, t: "1760000000000000000", n: 3 };
  const cursor = Buffer.from(JSON.stringify(Object.fromEntries(Array.from({ length: 50 }, (_, index) => [`${index}`.padStart(32, "0"), place])))).toString("base64url");
  const search = Schema.decodeUnknownOption(logHistorySchema)({ organizationSlug: "acme", environmentSlug: "production", projectSlug: "web", cursor });
  expect(Option.getOrUndefined(search)?.cursor).toBe(cursor);
});

type Landed = { names?: readonly string[] | null; failed?: true };

/** Stands in for the native handle: records each read as it lands, and plays a reader asking for one. */
class FakeNames {
  reads: Landed[] = [];
  // Like the native handle, a reader that asks before anyone waits is still heard.
  asked = false;
  wake?: () => void;
  want() { if (this.wake) this.wake(); else this.asked = true; }
  begin() {
    const read: Landed = {};
    this.reads.push(read);
    return { done: (names: readonly string[] | null) => { read.names = names; }, failed: () => { read.failed = true; } };
  }
  wanted() {
    if (this.asked) { this.asked = false; return Promise.resolve(); }
    return new Promise<void>(resolve => { this.wake = () => { this.wake = undefined; resolve(); }; });
  }
}

it("withholds a page's text when the Store can't say what is Sealed", async () => {
  const store = asTestDouble<ConfigStore>()({ read: async () => { throw new Error("the Store is down"); } });
  const handle = new FakeNames();
  const asked: unknown[] = [];
  // Plays the scrubbing reader: it asks for names, and once a read failed it hides each line and keeps the Store's gaps.
  const connected = asTestDouble<ConnectedRuntimeClient>()({
    logHistory: (options: LogHistoryOptions) => Effect.promise(async () => {
      asked.push(options.sealed);
      handle.want();
      await new Promise(resolve => setTimeout(resolve, 10));
      const failed = handle.reads.some(read => read.failed);
      const shown = { id: "m/c/100/history/0", source: asTestDouble<LogMetadata>()({ machine_id: "m", machine_name: "fsn-1", origin: { origin: "service" as const, service_id: "s", service_name: "api", container_id: "c", hook: null } }), timestamp_nanos: "100", channel: "stdout" as const, text: "shown" as const, level: "info" as const, message: "TOKEN=73519086427351908642" };
      const at = { machineId: "m", machineName: "fsn-1", containerId: "c", serviceName: "api" };
      return {
        records: failed ? [] : [shown],
        gaps: failed ? [{ ...at, fromNanos: "100", toNanos: "100", reason: "hidden" as const }] : [],
        exits: [],
        failures: [{ machineId: "m2", machineName: "fsn-2", message: "boom: TOKEN=73519086427351908642" }],
        cursor: null,
      };
    }),
  });
  const error = vi.spyOn(console, "error").mockImplementation(() => {});
  try {
    const read = await Effect.runPromise(Effect.scoped(readContainerLogs(connected, "org", { namespace: "app" }, new AbortController().signal, {}, asTestDouble<SealedNames>()(handle)))
      .pipe(Effect.provideService(CloudStore, { open: Effect.succeed(store) })));
    if (read.type !== "history") throw new Error("expected a page");
    expect(asked).toEqual([handle]);
    expect(handle.reads).toEqual([{ failed: true }]);
    expect(read.page.records).toEqual([]);
    expect(read.page.gaps.map(gap => gap.reason)).toEqual(["hidden"]);
    expect(read.page.failures.map(failure => failure.message)).toEqual(["Container logs are unavailable."]);
    expect(JSON.stringify(read.page)).not.toContain("7351908642");
  } finally { error.mockRestore(); }
});
