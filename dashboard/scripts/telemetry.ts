// Start the OpenTelemetry SDK only when the environment configures OpenTelemetry, as hosted Cloud
// does. Self-host sets nothing, so it runs no telemetry SDK and makes no telemetry calls. Any OTEL_*
// variable counts, even an empty one, so an environment that sets one starts the SDK as before. The
// images' own OTEL_NODE_RESOURCE_DETECTORS only tunes the SDK, so it doesn't count.
import { register } from "node:module";

const configured = Object.keys(process.env).some(
  (name) => name.startsWith("OTEL_") && name !== "OTEL_NODE_RESOURCE_DETECTORS",
);

if (configured) {
  // The loader hook must be registered before the app loads, or its ESM imports go uninstrumented.
  register("@opentelemetry/instrumentation/hook.mjs", import.meta.url);
  await import("@opentelemetry/auto-instrumentations-node/register");
}
