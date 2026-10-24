// Start the OpenTelemetry SDK only when the environment configures OpenTelemetry, as hosted Cloud
// does. Self-host sets nothing, so it runs no telemetry SDK and makes no telemetry calls. The images'
// own OTEL_NODE_RESOURCE_DETECTORS only tunes the SDK, so it doesn't count.
import { register } from "node:module";

const configured = Object.entries(process.env).some(
  ([name, value]) => name.startsWith("OTEL_") && name !== "OTEL_NODE_RESOURCE_DETECTORS" && value,
);

if (configured) {
  // The loader hook must be registered before the app loads, or its ESM imports go uninstrumented.
  register("@opentelemetry/instrumentation/hook.mjs", import.meta.url);
  await import("@opentelemetry/auto-instrumentations-node/register");
}
