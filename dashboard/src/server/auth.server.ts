import "@tanstack/react-start/server-only";
import { drizzleAdapter } from "@better-auth/drizzle-adapter";
import { checkout, polar, portal, webhooks } from "@polar-sh/better-auth";
import { Polar as PolarSdk } from "@polar-sh/sdk";
import { betterAuth } from "better-auth";
import { organization as organizationPlugin } from "better-auth/plugins";
import { tanstackStartCookies } from "better-auth/tanstack-start";
import { Context, Data, Effect, Layer, Redacted, Schema } from "effect";
import { getBetterAuthUrlConfig } from "#/auth/trusted-origins";
import {
  account,
  invitation,
  member,
  session,
  user,
  verification,
} from "#/modules/identity/tables";
import { organization } from "#/modules/organization/tables";
import {
  createOrganizationBillingSyncEventsFromCustomerStatePayload,
  createOrganizationBillingSyncEventsFromSubscriptionPayload,
  type InngestSendableEvent,
} from "#/modules/inngest/events";
import {
  InngestClient,
  sendInngestEvent,
} from "#/modules/inngest/client";
import {
  handleSessionCreated,
  handleUserCreated,
} from "#/modules/environment-design/workspace-bootstrap.server";
import { getOrganizationSlugById } from "#/modules/environment-design/workspace-repository.server";
import { Actor } from "#/modules/identity/actor";
import { Polar } from "#/modules/billing/polar-provider.server";
import { asString } from "#/lib/json";
import { AppConfig } from "#/server/config.server";
import { BetterAuthDatabase, Database } from "#/server/database.server";
import { Unauthorized } from "#/server/public-error";

export { Unauthorized };

export class AuthenticationUnavailable extends Data.TaggedError(
  "AuthenticationUnavailable",
)<{ readonly cause: unknown }> {
  readonly publicErrorCategory = "internal" as const;
}

const AuthSession = Schema.Struct({
  session: Schema.Struct({
    id: Schema.String,
    userId: Schema.String,
    activeOrganizationId: Schema.optionalKey(Schema.NullOr(Schema.String)),
    activeOrganizationSlug: Schema.optionalKey(Schema.NullOr(Schema.String)),
  }),
  user: Schema.Struct({
    id: Schema.String,
    email: Schema.String,
    name: Schema.String,
    image: Schema.optionalKey(Schema.NullOr(Schema.String)),
  }),
});

export type AuthSession = typeof AuthSession.Type;

export interface AuthService {
  readonly handler: (
    request: Request,
  ) => Effect.Effect<Response, AuthenticationUnavailable>;
  readonly getSession: (
    headers: Headers,
  ) => Effect.Effect<
    AuthSession | null,
    AuthenticationUnavailable | Schema.SchemaError
  >;
  readonly signInGithub: (
    headers: Headers,
    callbackURL: string,
  ) => Effect.Effect<Response, AuthenticationUnavailable>;
  readonly resolveActor: (
    headers: Headers,
  ) => Effect.Effect<
    Actor,
    Unauthorized | AuthenticationUnavailable | Schema.SchemaError
  >;
}

export class Auth extends Context.Service<Auth, AuthService>()("ployz/Auth") {}

const AuthSchema = {
  account,
  invitation,
  member,
  organization,
  session,
  user,
  verification,
};

function hostedPolarPlugin(
  config: Effect.Success<typeof AppConfig.make>,
  sendBillingEvents: {
    readonly subscription: (
      payload: Parameters<
        typeof createOrganizationBillingSyncEventsFromSubscriptionPayload
      >[0],
    ) => Promise<void>;
    readonly customer: (
      payload: Parameters<
        typeof createOrganizationBillingSyncEventsFromCustomerStatePayload
      >[0],
    ) => Promise<void>;
  },
) {
  if (config.polar.mode === "self_hosted") return null;
  const client = new PolarSdk({
    accessToken: Redacted.value(config.polar.accessToken),
    server: config.polar.server,
  });
  return polar({
    client,
    createCustomerOnSignUp: true,
    use: [
      checkout({
        products: [
          { productId: config.polar.productIds.free, slug: "free" },
          { productId: config.polar.productIds.solo, slug: "solo" },
          { productId: config.polar.productIds.teams, slug: "teams" },
        ],
        successUrl: config.polarSuccessUrl,
        authenticatedUsersOnly: true,
      }),
      portal({ returnUrl: new URL("/cloud", config.app.url).href }),
      webhooks({
        secret: Redacted.value(config.polar.webhookSecret),
        onSubscriptionCreated: sendBillingEvents.subscription,
        onSubscriptionUpdated: sendBillingEvents.subscription,
        onSubscriptionActive: sendBillingEvents.subscription,
        onSubscriptionCanceled: sendBillingEvents.subscription,
        onSubscriptionRevoked: sendBillingEvents.subscription,
        onSubscriptionUncanceled: sendBillingEvents.subscription,
        onCustomerStateChanged: sendBillingEvents.customer,
      }),
    ],
  });
}

const makeAuth = Effect.gen(function* () {
  const config = yield* AppConfig;
  const database = yield* BetterAuthDatabase;
  const applicationDatabase = yield* Database;
  const polarService = yield* Polar;
  const inngest = yield* InngestClient;
  const runHook = <A, E>(
    program: Effect.Effect<A, E, Database | Polar | InngestClient>,
  ) =>
    Effect.runPromise(
      program.pipe(
        Effect.provideService(Database, applicationDatabase),
        Effect.provideService(Polar, polarService),
        Effect.provideService(InngestClient, inngest),
      ),
    );
  const publishBillingEvents = (events: readonly InngestSendableEvent[]) =>
    events.length === 0
      ? Promise.resolve()
      : runHook(sendInngestEvent(events));
  const url = getBetterAuthUrlConfig(
    config.app.url.href,
    config.auth.trustedOrigins,
    { trustLocalhost: config.nodeEnv !== "production" },
  );
  const polarPlugin = hostedPolarPlugin(config, {
    subscription: (payload) =>
      publishBillingEvents(
        createOrganizationBillingSyncEventsFromSubscriptionPayload(payload),
      ),
    customer: (payload) =>
      publishBillingEvents(
        createOrganizationBillingSyncEventsFromCustomerStatePayload(payload),
      ),
  });
  const instance = betterAuth({
    secret: Redacted.value(config.auth.secret),
    baseURL: {
      allowedHosts: url.allowedHosts,
      fallback: url.fallbackURL,
      protocol: "auto",
    },
    database: drizzleAdapter(database.drizzle, {
      provider: "pg",
      schema: AuthSchema,
    }),
    trustedOrigins: url.trustedOrigins,
    emailAndPassword: { enabled: config.nodeEnv !== "production" },
    socialProviders: {
      github: {
        clientId: config.github.clientId,
        clientSecret: Redacted.value(config.github.clientSecret),
      },
    },
    session: {
      additionalFields: {
        activeOrganizationSlug: { type: "string", required: false },
      },
    },
    advanced: { database: { generateId: "uuid" } },
    databaseHooks: {
      user: {
        create: { after: (createdUser) => runHook(handleUserCreated(createdUser)) },
      },
      session: {
        create: {
          after: (createdSession, context) =>
            runHook(handleSessionCreated(createdSession, context)),
        },
        update: {
          async before(sessionRecord) {
            if (!("activeOrganizationId" in sessionRecord)) return;
            const activeOrganizationId = asString(
              sessionRecord["activeOrganizationId"],
            );
            return {
              data: {
                ...sessionRecord,
                activeOrganizationSlug: activeOrganizationId === null
                  ? null
                  : await runHook(getOrganizationSlugById(activeOrganizationId)),
              },
            };
          },
        },
      },
    },
    plugins: [
      organizationPlugin(),
      ...(polarPlugin === null ? [] : [polarPlugin]),
      tanstackStartCookies(),
    ],
  });
  yield* Effect.tryPromise({
    try: () => instance.$context,
    catch: (cause) => new AuthenticationUnavailable({ cause }),
  });

  const getSession = Effect.fn("Auth.getSession")(function* (headers: Headers) {
    const authSession = yield* Effect.tryPromise({
      try: () => instance.api.getSession({ headers }),
      catch: (cause) => new AuthenticationUnavailable({ cause }),
    });
    if (authSession === null) return null;
    return yield* Schema.decodeUnknownEffect(AuthSession)(authSession);
  });

  return {
    handler: (request) => Effect.tryPromise({
      try: () => instance.handler(request),
      catch: (cause) => new AuthenticationUnavailable({ cause }),
    }),
    getSession,
    signInGithub: (headers, callbackURL) => Effect.tryPromise({
      try: () => instance.api.signInSocial({
        headers,
        body: { provider: "github", callbackURL },
        asResponse: true,
      }),
      catch: (cause) => new AuthenticationUnavailable({ cause }),
    }),
    resolveActor: Effect.fn("Auth.resolveActor")(function* (headers: Headers) {
      const authSession = yield* getSession(headers);
      if (authSession === null) return yield* new Unauthorized();
      return yield* Schema.decodeUnknownEffect(Actor)({
        userId: authSession.user.id,
      }, { onExcessProperty: "error" });
    }),
  } satisfies AuthService;
});

export const AuthLive = Layer.effect(Auth, makeAuth);
