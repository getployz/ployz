import "@tanstack/react-start/server-only";
import {
  Config,
  Context,
  Data,
  Effect,
  Layer,
  Option,
  type Redacted,
  Result,
  Schema,
  SchemaIssue,
} from "effect";

const Uuid = Schema.String.check(Schema.isUUID());
const DEFAULT_HOSTED_DNS_URL = new URL("https://dns.ployz.app/");
const DEFAULT_POSTHOG_HOST = new URL("https://us.i.posthog.com");
export type GithubOrigins = { readonly api: string; readonly web: string; readonly codeload: string; readonly oidc: string };
const GITHUB_ORIGINS: GithubOrigins = {
  api: "https://api.github.com",
  web: "https://github.com",
  codeload: "https://codeload.github.com",
  oidc: "https://token.actions.githubusercontent.com",
};
const NonEmptySecret = Schema.Redacted(Schema.NonEmptyString);
const EncryptionSecret = Schema.Redacted(
  Schema.String.check(Schema.isMinLength(32)),
);

export type PolarConfiguration =
  | { readonly mode: "self_hosted" }
  | {
      readonly mode: "hosted";
      readonly accessToken: Redacted.Redacted<string>;
      readonly webhookSecret: Redacted.Redacted<string>;
      readonly server: "production" | "sandbox";
      readonly productId: string;
    };

/** PostShiba sends Cloud's email; null (every variable unset) is Off. */
export type MailConfiguration = {
  /** A verify lab's fake PostShiba; null sends to PostShiba. */
  readonly verifyOrigin: string | null;
  readonly apiKey: Redacted.Redacted<string>;
  readonly teamId: string;
  readonly clusterId: string;
  readonly from: string;
} | null;

export class InvalidConfiguration extends Data.TaggedError(
  "InvalidConfiguration",
)<{ readonly message: string }> {}

const optional = <A>(config: Config.Config<A>) =>
  Config.option(config).pipe(Config.map(Option.getOrUndefined));

const rawFields = {
  nodeEnv: Config.literals(["development", "test", "production"], "NODE_ENV").pipe(
    Config.withDefault("production"),
  ),
  databaseUrl: Config.url("DATABASE_URL"),
  appUrl: Config.url("APP_URL"),
  port: Config.port("PORT").pipe(Config.withDefault(3000)),
  betterAuthSecret: Config.schema(NonEmptySecret, "BETTER_AUTH_SECRET"),
  betterAuthTrustedOrigins: optional(
    Config.nonEmptyString("BETTER_AUTH_TRUSTED_ORIGINS"),
  ),
  // Railway injects this; its edge sets X-Real-IP to the client's address, overwriting any sent.
  railwayEnvironmentId: optional(Config.nonEmptyString("RAILWAY_ENVIRONMENT_ID")),
  githubClientId: Config.nonEmptyString("GITHUB_CLIENT_ID"),
  githubClientSecret: Config.schema(NonEmptySecret, "GITHUB_CLIENT_SECRET"),
  githubAppId: Config.nonEmptyString("GITHUB_APP_ID"),
  githubAppPrivateKey: Config.schema(NonEmptySecret, "GITHUB_APP_PRIVATE_KEY"),
  githubAppSlug: Config.nonEmptyString("GITHUB_APP_SLUG"),
  githubAppClientId: Config.nonEmptyString("GITHUB_APP_CLIENT_ID"),
  githubAppClientSecret: Config.schema(NonEmptySecret, "GITHUB_APP_CLIENT_SECRET"),
  githubAppWebhookSecret: Config.schema(
    NonEmptySecret,
    "GITHUB_APP_WEBHOOK_SECRET",
  ),
  verifyGithubOrigin: optional(Config.url("PLOYZ_VERIFY_GITHUB_ORIGIN")),
  verifyPostshibaOrigin: optional(Config.url("PLOYZ_VERIFY_POSTSHIBA_ORIGIN")),
  polarAccessToken: optional(
    Config.schema(NonEmptySecret, "POLAR_ACCESS_TOKEN"),
  ),
  polarServer: Config.literals(["production", "sandbox"], "POLAR_SERVER").pipe(
    Config.withDefault("production"),
  ),
  polarWebhookSecret: optional(
    Config.schema(NonEmptySecret, "POLAR_WEBHOOK_SECRET"),
  ),
  polarProductId: optional(Config.schema(Uuid, "POLAR_PRODUCT_ID")),
  posthogKey: optional(Config.nonEmptyString("POSTHOG_KEY")),
  posthogHost: Config.url("POSTHOG_HOST").pipe(
    Config.withDefault(DEFAULT_POSTHOG_HOST),
  ),
  hostedDnsUrl: Config.url("PLOYZ_HOSTED_DNS_URL").pipe(
    Config.withDefault(DEFAULT_HOSTED_DNS_URL),
  ),
  hostedDnsMintKey: optional(
    Config.schema(NonEmptySecret, "PLOYZ_HOSTED_DNS_MINT_KEY"),
  ),
  postshibaApiKey: optional(Config.schema(NonEmptySecret, "POSTSHIBA_API_KEY")),
  postshibaTeamId: optional(Config.nonEmptyString("POSTSHIBA_TEAM_ID")),
  postshibaClusterId: optional(Config.nonEmptyString("POSTSHIBA_CLUSTER_ID")),
  mailFrom: optional(Config.nonEmptyString("MAIL_FROM")),
  marketingOrigin: optional(Config.url("MARKETING_ORIGIN")),
  uptimeEnabled: Config.boolean("UPTIME_ENABLED").pipe(Config.withDefault(false)),
  analyticsEnabled: Config.boolean("ANALYTICS_ENABLED").pipe(Config.withDefault(false)),
  // Self-host never sets these: without ClickHouse, ingest keeps no outbox and every site reads Postgres.
  analyticsStore: Config.literals(["postgres", "clickhouse"], "ANALYTICS_STORE").pipe(Config.withDefault("postgres" as const)),
  clickhouseUrl: optional(Config.schema(NonEmptySecret, "CLICKHOUSE_URL")),
  inngestEventKey: Config.schema(NonEmptySecret, "INNGEST_EVENT_KEY"),
  inngestSigningKey: Config.schema(NonEmptySecret, "INNGEST_SIGNING_KEY"),
  // The Inngest SDK also reads INNGEST_BASE_URL.
  inngestBaseUrl: Config.url("INNGEST_BASE_URL").pipe(Config.withDefault(new URL("https://api.inngest.com"))),
  encryptionSecret: Config.schema(EncryptionSecret, "APP_ENCRYPTION_SECRET"),
  supportStaffUserIds: optional(Config.nonEmptyString("PLOYZ_SUPPORT_STAFF_USER_IDS")),
};
/**
 * Config.all stops at the first bad variable. Read every field once so one
 * ConfigError names all missing or malformed variables.
 */
const loadRawConfig = Effect.all(rawFields, { mode: "result" }).pipe(
  Effect.flatMap((results) => {
    const issues: Array<SchemaIssue.Issue> = [];
    for (const result of Object.values(results)) {
      if (result._tag === "Success") continue;
      const { cause } = result.failure;
      issues.push(
        Schema.isSchemaError(cause)
          ? cause.issue
          : new SchemaIssue.InvalidValue({ message: cause.message }),
      );
    }
    const [head, ...tail] = issues;
    return head === undefined
      ? Effect.fromResult(Result.all(results))
      : Effect.fail(
          new Config.ConfigError(
            new Schema.SchemaError(
              new SchemaIssue.Composite(Schema.Unknown.ast, [head, ...tail]),
            ),
          ),
        );
  }),
);

/** No Polar variables means a Self-hosted Cloud; a partial set is a mistake. */
const resolvePolarConfiguration = Effect.fn("Config.resolvePolar")(function* (
  input: Effect.Success<typeof loadRawConfig>,
) {
  const { polarAccessToken, polarWebhookSecret, polarProductId } = input;
  if (
    polarAccessToken === undefined &&
    polarWebhookSecret === undefined &&
    polarProductId === undefined
  ) {
    return { mode: "self_hosted" } as const;
  }
  if (
    polarAccessToken === undefined ||
    polarWebhookSecret === undefined ||
    polarProductId === undefined
  ) {
    return yield* new InvalidConfiguration({
      message:
        "Polar configuration must be entirely absent or include POLAR_ACCESS_TOKEN, POLAR_WEBHOOK_SECRET, and POLAR_PRODUCT_ID",
    });
  }
  return {
    mode: "hosted",
    accessToken: polarAccessToken,
    webhookSecret: polarWebhookSecret,
    server: input.polarServer,
    productId: polarProductId,
  } as const;
});

/** No mail variables means no email; a partial set is a mistake. */
const resolveMailConfiguration = Effect.fn("Config.resolveMail")(function* (
  input: Effect.Success<typeof loadRawConfig>,
) {
  const { postshibaApiKey: apiKey, postshibaTeamId: teamId, postshibaClusterId: clusterId, mailFrom: from } = input;
  if (apiKey === undefined && teamId === undefined && clusterId === undefined && from === undefined) {
    return null satisfies MailConfiguration;
  }
  if (apiKey === undefined || teamId === undefined || clusterId === undefined || from === undefined) {
    return yield* new InvalidConfiguration({
      message:
        "Mail configuration must be entirely absent or include POSTSHIBA_API_KEY, POSTSHIBA_TEAM_ID, POSTSHIBA_CLUSTER_ID, and MAIL_FROM",
    });
  }
  const origin = yield* resolveVerifyOrigin(input.nodeEnv, "PLOYZ_VERIFY_POSTSHIBA_ORIGIN", input.verifyPostshibaOrigin);
  return { verifyOrigin: origin ?? null, apiKey, teamId, clusterId, from } satisfies MailConfiguration;
});

const resolveVerifyOrigin = Effect.fn("Config.resolveVerifyOrigin")(function* (
  nodeEnv: Effect.Success<typeof loadRawConfig>["nodeEnv"],
  name: string,
  origin: URL | undefined,
) {
  if (origin === undefined) return undefined;
  if (nodeEnv === "production") {
    return yield* new InvalidConfiguration({ message: `${name} is for verify labs and is refused when NODE_ENV is production` });
  }
  const bare = `${origin.origin}/`;
  if ((origin.protocol !== "http:" && origin.protocol !== "https:") || origin.href !== bare) {
    return yield* new InvalidConfiguration({
      message: `${name} must be an http(s) origin with no path, query or credentials, like http://127.0.0.1:4555, not ${origin.href}`,
    });
  }
  yield* Effect.logInfo(`${name} ${origin.origin}`);
  return origin.origin;
});

const resolveGithubOrigins = Effect.fn("Config.resolveGithubOrigins")(function* (
  input: Effect.Success<typeof loadRawConfig>,
) {
  const origin = yield* resolveVerifyOrigin(input.nodeEnv, "PLOYZ_VERIFY_GITHUB_ORIGIN", input.verifyGithubOrigin);
  if (origin === undefined) return GITHUB_ORIGINS;
  return { api: origin, web: origin, codeload: origin, oidc: origin } satisfies GithubOrigins;
});

/** Comma-separated user ids, taken exactly: no email or domain matching. */
const parseUserIds = (value: string | undefined): ReadonlyArray<string> =>
  value === undefined ? [] : value.split(",").map((id) => id.trim()).filter((id) => id !== "");

const makeAppConfig = Effect.gen(function* () {
  const raw = yield* loadRawConfig;
  const polar = yield* resolvePolarConfiguration(raw);
  const mail = yield* resolveMailConfiguration(raw);
  const githubOrigins = yield* resolveGithubOrigins(raw);

  return {
    nodeEnv: raw.nodeEnv,
    app: { url: raw.appUrl, port: raw.port },
    database: { url: raw.databaseUrl },
    auth: {
      secret: raw.betterAuthSecret,
      trustedOrigins: raw.betterAuthTrustedOrigins,
      url: raw.appUrl,
      // Elsewhere better-auth's default: a single-address X-Forwarded-For.
      clientIpHeaders: raw.railwayEnvironmentId === undefined ? undefined : ["x-real-ip"],
    },
    github: {
      clientId: raw.githubClientId,
      clientSecret: raw.githubClientSecret,
      appId: raw.githubAppId,
      appPrivateKey: raw.githubAppPrivateKey,
      appSlug: raw.githubAppSlug,
      appClientId: raw.githubAppClientId,
      appClientSecret: raw.githubAppClientSecret,
      appWebhookSecret: raw.githubAppWebhookSecret,
      origins: githubOrigins,
    },
    polar,
    // Unset (Self-hosted Cloud): no product analytics. The project key is public; browsers send it too.
    posthog: raw.posthogKey === undefined
      ? null
      : { key: raw.posthogKey, host: raw.posthogHost.origin },
    mail,
    ployz: {
      hostedDnsUrl: raw.hostedDnsUrl,
      hostedDnsMintKey: raw.hostedDnsMintKey,
    },
    inngest: {
      eventKey: raw.inngestEventKey,
      signingKey: raw.inngestSigningKey,
      baseUrl: raw.inngestBaseUrl,
    },
    // Unset (Self-hosted Cloud): no marketing proxy; / sends signed-out visitors to /auth.
    marketingOrigin: raw.marketingOrigin,
    // Unset (Self-hosted Cloud): no uptime checker runs, so the dashboard shows no uptime.
    uptimeEnabled: raw.uptimeEnabled,
    analyticsEnabled: polar.mode !== "self_hosted" || raw.analyticsEnabled,
    // Unset (Self-hosted Cloud): analytics lives only in Postgres. `reads` lets sites whose copy passed parity read it.
    clickhouse: raw.clickhouseUrl === undefined ? null : { url: raw.clickhouseUrl, reads: raw.analyticsStore === "clickhouse" },
    encryptionSecret: raw.encryptionSecret,
    // Ployz support exists only on Ployz-hosted Cloud: a Self-hosted Cloud never has staff, whatever the variable says.
    supportStaffUserIds: polar.mode === "self_hosted" ? [] : parseUserIds(raw.supportStaffUserIds),
  } as const;
});

export class AppConfig extends Context.Service<AppConfig>()("ployz/AppConfig", {
  make: makeAppConfig,
}) {
  static readonly layer = Layer.effect(this, this.make);
}
