import "@tanstack/react-start/server-only";
import * as OtelTracer from "@effect/opentelemetry/OtelTracer";
import * as Resource from "@effect/opentelemetry/Resource";
import { Layer, ManagedRuntime } from "effect";
import { OrganizationRuntimeLive } from "#/modules/runtime/organization-runtime.server";
import { PloyzLive } from "#/modules/runtime/ployz.server";
import { PolarLive } from "#/modules/billing/polar-provider.server";
import { GithubApiLive } from "#/modules/github/github-observation.api";
import { GithubOidcKeysLive } from "#/modules/github/github-oidc.server";
import { InngestLive } from "#/modules/inngest/client";
import { AuthLive } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";
import { DatabaseLive, ReportingDatabaseLive } from "#/server/database.server";
import { SecretEncryptionLive } from "#/utils/encrypted-secret.server";

const InfrastructureLive = Layer.mergeAll(
  SecretEncryptionLive,
  PolarLive,
  InngestLive,
  GithubApiLive,
  GithubOidcKeysLive,
  ReportingDatabaseLive,
).pipe(
  Layer.provideMerge(DatabaseLive),
  Layer.provideMerge(PloyzLive),
  Layer.provideMerge(AppConfig.layer),
);

const RuntimeLive = OrganizationRuntimeLive.pipe(
  Layer.provideMerge(InfrastructureLive),
);

// Effect spans go through the OpenTelemetry SDK the start command registers,
// so they nest under the HTTP server span and share its exporter. Without the
// SDK the global provider is a no-op and this costs nothing. The Resource only
// names the tracer; an unnamed instrumentation scope crashes the OTLP exporter.
const TracingLive = OtelTracer.layerGlobal.pipe(
  Layer.provide(Resource.layer({ serviceName: "ployz-cloud" })),
);

export const AppLive = AuthLive.pipe(
  Layer.provideMerge(InfrastructureLive),
  Layer.merge(RuntimeLive),
  Layer.provideMerge(TracingLive),
);

export type AppServices = Layer.Success<typeof AppLive>;

export const AppRuntime = ManagedRuntime.make(AppLive);
