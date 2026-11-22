import { getRequestIP } from "@tanstack/react-start/server";
import { Effect, Schema } from "effect";
import { MANIFEST_HEADER, MANIFEST_REVISION } from "#/modules/commands/commands.server";
import { organizationSlugSchema } from "#/modules/organization/tables";
import { AppConfig } from "#/server/config.server";
import { ClientAddress, observedIp } from "#/server/client-ip";
import { methodNotAllowedFor, publicErrorResponse, Validation } from "#/server/public-error";
import { runAppEffect } from "#/server/run.server";
import type { AppServices } from "#/server/runtime.server";

type Method = "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS" | "HEAD";

export interface ApiContext<P = Record<never, never>> {
  readonly request: Request;
  readonly params: P;
}

type ApiHandler<P> = (context: ApiContext<P>) => Effect.Effect<Response, unknown, AppServices>;
type ServerHandler<P> = (context: ApiContext<P>) => Promise<Response>;

/** Each method a route serves as an Effect, and `ANY` only when the route answers every method itself. */
type ApiHandlers<P> = Partial<Record<Method | "ANY", ApiHandler<P>>>;

interface ApiRouteOptions {
  /** The caller's address (`ClientAddress`) is read from the connection before the handler runs. */
  readonly clientAddress?: true;
  /** Every reply, a failure too, names the CLI manifest revision. */
  readonly manifest?: true;
  /** What a method no handler takes gets, for a route whose clients read its own refusal. */
  readonly refuseOthers?: () => Response;
}

type RouteHandlers = Readonly<Partial<Record<string, ServerHandler<never>>>>;

const apiRoutes = new WeakSet<RouteHandlers>();

/** Whether `handlers` came from {@link apiRoute} or {@link cliRoute}. */
export const isApiRoute = (handlers: RouteHandlers) => apiRoutes.has(handlers);

function withManifest(response: Response) {
  const headers = new Headers(response.headers);
  headers.set(MANIFEST_HEADER, MANIFEST_REVISION);
  return new Response(response.body, { status: response.status, statusText: response.statusText, headers });
}

/** `effect` knowing the address `request` came from, which a Caller's failed-authentication limit keys on. */
function fromClient<A, E, R>(request: Request, peer: string | undefined, effect: Effect.Effect<A, E, R>) {
  return Effect.gen(function* () {
    const { auth } = yield* AppConfig;
    return yield* Effect.provideService(effect, ClientAddress, observedIp(request.headers, auth.clientIpHeaders, peer));
  });
}

/** The address of the client this request came from, for a route built with `clientAddress`. */
export const clientIp = Effect.map(ClientAddress, (address) => address ?? "unknown");

function serving<P>(handler: ApiHandler<P>, options: ApiRouteOptions): ServerHandler<P> {
  return async ({ request, params }) => {
    const peer = options.clientAddress === true ? getRequestIP() : undefined;
    const reply = handler({ request, params });
    const response = await runAppEffect(options.clientAddress === true ? fromClient(request, peer, reply) : reply, {
      signal: request.signal,
    }).catch(publicErrorResponse);
    return options.manifest === true ? withManifest(response) : response;
  };
}

type Served<H, P> = { readonly [M in keyof H]: ServerHandler<P> } & { readonly ANY: ServerHandler<P> };

/**
 * The server handlers of an API route: each method is an Effect run on the application runtime, any failure answers
 * as its public error, and a method nothing here takes gets a 405 naming those that are.
 */
export function apiRoute<P = Record<never, never>, H extends ApiHandlers<P> = ApiHandlers<P>>(
  handlers: H,
  options: ApiRouteOptions = {},
): Served<H, P> {
  const served: Partial<Record<Method | "ANY", ServerHandler<P>>> = {};
  const methods: Array<string> = [];
  // SAFETY: `H extends ApiHandlers<P>`, so its keys are methods and its values are handlers.
  for (const [method, handler] of Object.entries(handlers) as Array<[Method | "ANY", ApiHandler<P>]>) {
    served[method] = serving(handler, options);
    if (method !== "ANY") methods.push(method);
  }
  served.ANY ??= () => Promise.resolve(options.refuseOthers?.() ?? methodNotAllowedFor(methods));
  apiRoutes.add(served);
  // SAFETY: `served` holds a handler for every key of `H` (the loop above) and an `ANY` (assigned above).
  return served as Served<H, P>;
}

/** {@link apiRoute} for a CLI call: the client address is known and every reply names the manifest revision. */
export const cliRoute = <P = Record<never, never>, H extends ApiHandlers<P> = ApiHandlers<P>>(handlers: H) =>
  apiRoute<P, H>(handlers, { clientAddress: true, manifest: true });

/** A redirect the browser follows with a GET, never cached. */
export const seeOther = (location: string) => new Response(null, { status: 303, headers: { location, "cache-control": "no-store" } });

/** A page of `html`, never cached. */
export const htmlPage = (status: number, html: string) =>
  new Response(html, { status, headers: { "content-type": "text/html; charset=utf-8", "cache-control": "no-store" } });

/** `request`'s query as `schema` decodes it, or `refusal` (a Validation) when it does not fit. */
export const decodeSearch = <S extends Schema.Decoder<unknown>>(schema: S, request: Request, refusal: Validation) =>
  Schema.decodeUnknownEffect(schema)(Object.fromEntries(new URL(request.url).searchParams)).pipe(
    Effect.mapError(() => refusal),
  );

/** `request`'s JSON body as `schema` decodes it, or `refusal` (a Validation) when it is no JSON or does not fit. */
export const decodeJsonBody = <S extends Schema.Decoder<unknown>>(schema: S, request: Request, refusal: Validation) =>
  Effect.tryPromise({ try: () => request.json(), catch: () => refusal }).pipe(
    Effect.catch(() => Effect.succeed(null)),
    Effect.flatMap((body) => Schema.decodeUnknownEffect(schema)(body)),
    Effect.mapError(() => refusal),
  );

const OrganizationSearch = Schema.Struct({ organizationSlug: organizationSlugSchema });

/** {@link apiRoute} serving GET for the Organization `?organizationSlug=` names; a missing or invalid slug is refused first. */
export const organizationSearchRoute = (
  handler: (request: Request, organizationSlug: string) => Effect.Effect<Response, unknown, AppServices>,
) =>
  apiRoute({
    GET: ({ request }) =>
      Effect.flatMap(
        decodeSearch(OrganizationSearch, request, new Validation({ message: "A valid organization slug is required" })),
        ({ organizationSlug }) => handler(request, organizationSlug),
      ),
  });
