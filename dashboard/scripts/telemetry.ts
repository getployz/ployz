// Start the OpenTelemetry SDK only when there is somewhere to send it. Hosted Cloud sets an OTLP
// endpoint; self-host sets none, so it runs no telemetry SDK and makes no telemetry calls.
import { register } from "node:module";

const endpoints = [
  "OTEL_EXPORTER_OTLP_ENDPOINT",
  "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
  "OTEL_EXPORTER_OTLP_METRICS_ENDPOINT",
  "OTEL_EXPORTER_OTLP_LOGS_ENDPOINT",
];

// An exporter named outright, such as console, also sends somewhere; "none" doesn't.
const exporters = ["OTEL_TRACES_EXPORTER", "OTEL_METRICS_EXPORTER", "OTEL_LOGS_EXPORTER"];

if (
  endpoints.some((name) => process.env[name]) ||
  exporters.some((name) => {
    const value = process.env[name];
    return value !== undefined && value !== "" && value !== "none";
  })
) {
  // The loader hook must be registered before the app loads, or its ESM imports go uninstrumented.
  register("@opentelemetry/instrumentation/hook.mjs", import.meta.url);
  await import("@opentelemetry/auto-instrumentations-node/register");
}
