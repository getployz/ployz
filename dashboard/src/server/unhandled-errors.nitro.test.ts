import { DrizzleQueryError } from "drizzle-orm";
import { useNitroApp, useNitroHooks } from "nitro/app";
import { formatWithOptions } from "node:util";
import { afterEach, expect, it, vi } from "vitest";
// Nitro's server entry calls this trap once its app has run Cloud's plugins.
import { trapUnhandledErrors } from "../../node_modules/nitro/dist/runtime/internal/error/hooks.mjs";
import redactUnhandledErrors from "#/server/unhandled-errors.nitro";

const FAKE_SESSION_TOKEN = "fake-session-token-0123456789abc";

const EVENTS = ["unhandledRejection", "uncaughtException"] as const;

afterEach(() => {
  vi.restoreAllMocks();
});

it("keeps a failed query's params out of the log when Nitro traps an unhandled rejection", async () => {
  const lines: string[] = [];
  const record = (...args: unknown[]) => {
    lines.push(formatWithOptions({ depth: Infinity }, ...args));
  };
  for (const method of ["log", "info", "warn", "error", "debug", "trace"] as const) {
    vi.spyOn(console, method).mockImplementation(record);
  }
  for (const stream of [process.stdout, process.stderr]) {
    vi.spyOn(stream, "write").mockImplementation((chunk: string | Uint8Array) => {
      lines.push(Buffer.from(chunk).toString("utf8"));
      return true;
    });
  }
  const before = new Map(EVENTS.map((event) => [event, process.listeners(event)]));
  redactUnhandledErrors(Object.assign(useNitroApp(), { hooks: useNitroHooks() }));
  trapUnhandledErrors();
  try {
    process.emit("unhandledRejection", new DrizzleQueryError('select "id" from "session" where "token" = $1', [FAKE_SESSION_TOKEN]), Promise.resolve());
    await vi.waitFor(() => expect(lines.join("\n")).toContain("DrizzleQueryError in select session"));
  } finally {
    for (const event of EVENTS) {
      for (const listener of process.listeners(event)) {
        if (!before.get(event)?.includes(listener)) process.off(event, listener);
      }
    }
  }

  expect(lines.join("\n")).not.toContain(FAKE_SESSION_TOKEN);
  expect(lines.join("\n")).toContain("[unhandledRejection]");
});
