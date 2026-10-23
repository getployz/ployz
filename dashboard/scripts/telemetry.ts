// Start OpenTelemetry only when there is somewhere to send it. Hosted Cloud sets an OTLP endpoint;
// self-host sets none, so it runs no telemetry SDK and makes no telemetry calls.
import { register } from "node:module";

const endpoints = [
  "OTEL_EXPORTER_OTLP_ENDPOINT",
  "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
  "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
  "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
];

if (endpoints.some((name) => process.env[name])) {
  // The loader hook must be registered before the app loads, or its ESM imports go uninstrumented.
  register("@opentelemetry/instrumentation/hook.mjs", import.meta.url);
  await import("@opentelemetry/auto-instrumentations-node/register");
}
