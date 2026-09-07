import { ELECTRIC_PROTOCOL_QUERY_PARAMS } from "@electric-sql/client";
import { Data, Effect, Redacted } from "effect";
import { getPloyzTable } from "#/electric/synced-tables.server";
import { getOrganizationForUserBySlug } from "#/modules/environment-design/workspace-repository.server";
import { Auth } from "#/server/auth.server";
import { AppConfig } from "#/server/config.server";

const exposedElectricHeaders = [
  "electric-offset",
  "electric-handle",
  "electric-schema",
  "electric-cursor",
].join(", ");

export class TableSyncNotFound extends Data.TaggedError("NotFound")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "not-found" as const;
}

export class TableSyncUnauthorized extends Data.TaggedError("Unauthorized")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "unauthorized" as const;
}

export class TableSyncValidation extends Data.TaggedError("SchemaError")<{
  readonly message: string;
}> {
  readonly publicErrorCategory = "validation" as const;
}

export class TableSyncProviderFailure extends Data.TaggedError(
  "TableSyncProviderFailure",
)<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

export const handleTableSyncRequest = Effect.fn("Electric.handleTableSync")(
  function* (
    request: Request,
    tableName: string,
  ) {
    const table = getPloyzTable(tableName);
    if (!table) {
      return yield* new TableSyncNotFound({ message: "Table not found" });
    }

    const auth = yield* Auth;
    const session = yield* auth.getSession(request.headers).pipe(
      Effect.mapError((cause) => new TableSyncProviderFailure({ cause })),
    );
    if (!session) {
      return yield* new TableSyncUnauthorized({
        message: "Authentication is required.",
      });
    }

    let scopeId = session.user.id;
    if (table.scope === "organization") {
      const organizationSlug = new URL(request.url).searchParams.get(
        "organizationSlug",
      );
      if (!organizationSlug) {
        return yield* new TableSyncValidation({
          message: "organizationSlug is required",
        });
      }
      const organization = yield* getOrganizationForUserBySlug(
        session.user.id,
        organizationSlug,
      ).pipe(
        Effect.mapError((cause) => new TableSyncProviderFailure({ cause })),
      );
      if (!organization) {
        return yield* new TableSyncNotFound({
          message: "Organization not found",
        });
      }
      scopeId = organization.id;
    }

    const config = yield* AppConfig;
    const incomingUrl = new URL(request.url);
    const electricUrl = new URL("/v1/shape", config.electric.url);
    for (const key of ELECTRIC_PROTOCOL_QUERY_PARAMS) {
      for (const value of incomingUrl.searchParams.getAll(key)) {
        electricUrl.searchParams.append(key, value);
      }
    }
    electricUrl.searchParams.set("table", tableName);
    electricUrl.searchParams.set("where", `${table.whereColumn} = $1`);
    electricUrl.searchParams.set("params[1]", scopeId);
    if (table.columns) {
      electricUrl.searchParams.set(
        "columns",
        table.columns.map((column) => `"${column}"`).join(","),
      );
    }
    if (config.electric.sourceId) {
      electricUrl.searchParams.set("source_id", config.electric.sourceId);
    }
    if (config.electric.secret) {
      electricUrl.searchParams.set(
        "secret",
        Redacted.value(config.electric.secret),
      );
    }

    const body = request.method === "POST"
      ? yield* Effect.tryPromise({
          try: () => request.arrayBuffer(),
          catch: (cause) => new TableSyncProviderFailure({ cause }),
        })
      : undefined;
    const upstream = yield* Effect.tryPromise({
      try: () =>
        fetch(electricUrl, {
          method: request.method,
          headers:
            request.method === "POST"
              ? {
                  "content-type":
                    request.headers.get("content-type") ?? "application/json",
                }
              : undefined,
          body,
          signal: request.signal,
        }),
      catch: (cause) => new TableSyncProviderFailure({ cause }),
    });
    const headers = new Headers(upstream.headers);
    headers.delete("content-encoding");
    headers.delete("content-length");
    headers.set("access-control-expose-headers", exposedElectricHeaders);
    headers.set("cache-control", "private, no-store");

    return new Response(upstream.body, {
      status: upstream.status,
      statusText: upstream.statusText,
      headers,
    });
  },
);
