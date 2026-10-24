import { Effect } from "effect";
import { definePlugin } from "nitro";
import { runRedactedLog } from "#/server/log-redaction";

const TRAPPED = new Set(["unhandledRejection", "uncaughtException"]);

// Nitro's trap hands each unhandled error to `captureError`; patched, it no longer prints the error raw first.
export default definePlugin((nitroApp) => {
  const capture = nitroApp.captureError;
  nitroApp.captureError = (error, context) => {
    const tag = context.tags?.find((name) => TRAPPED.has(name));
    if (tag !== undefined) runRedactedLog(Effect.logError(`[${tag}]`, error));
    capture?.(error, context);
  };
});
