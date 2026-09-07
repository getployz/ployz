import {
  createInsertSchema,
  createSelectSchema,
} from "drizzle-orm/effect-schema";
import { Effect, Schema, SchemaGetter } from "effect";
import {
  environmentCanvasNodePosition,
  REGISTRY_CREDENTIAL_AUTH_MODES,
  REGISTRY_CREDENTIAL_PROVIDERS,
  SERVICE_SOURCE_TYPES,
  service,
} from "#/modules/environment-design/tables";
import type {
  ServiceBuildConfig,
  ServiceRestartPolicy as ServiceRestartPolicyRecord,
} from "#/modules/environment-design/tables";
import { encryptedSecretValueSchema } from "#/modules/environment-design/variables";
import {
  EnvironmentSlug,
  OrganizationSlug,
  ProjectSlug,
  Uuid,
} from "#/modules/environment-design/workspace-schemas";
import type { DeepMutable } from "#/modules/environment-design/schema";

export {
  SERVICE_SOURCE_TYPES,
  type ServiceSourceType,
} from "#/modules/environment-design/tables";

const requiredTrimmedString = Schema.Trim.check(Schema.isNonEmpty());
const serviceName = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Service name is required" }),
  Schema.isMaxLength(64, {
    message: "Service names must be 64 characters or fewer",
  }),
);

export const serviceRootDirSchema = Schema.Trim.pipe(
  Schema.check(
    Schema.isNonEmpty({ message: "Root directory is required" }),
    Schema.isStartsWith("/", { message: "Root directory must start with /" }),
    Schema.isPattern(/^\/[A-Za-z0-9._/-]*$/, {
      message: "Root directory can only include letters, numbers, ., _, -, and /",
    }),
    Schema.makeFilter(
      (value: string) => !value.includes("//"),
      { message: "Root directory cannot contain //" },
    ),
  ),
  Schema.decode({
    decode: SchemaGetter.transform((value) =>
      value === "/" ? "/" : value.replace(/\/+$/, ""),
    ),
    encode: SchemaGetter.transform((value) => value),
  }),
);

const serviceRepositorySchema = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Repository is required" }),
  Schema.isMaxLength(300),
);
const serviceBranchNameSchema = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Branch name is required" }),
  Schema.isMaxLength(255),
);
const serviceImageSchema = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Image is required" }),
  Schema.isMaxLength(500),
);
const serviceTagSchema = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Tag is required" }),
  Schema.isMaxLength(255),
);

const registryCredentialProviderSchema = Schema.Literals(
  REGISTRY_CREDENTIAL_PROVIDERS,
);
const registryCredentialAuthModeSchema = Schema.Literals(
  REGISTRY_CREDENTIAL_AUTH_MODES,
);

export const registryCredentialUsernameSchema = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Username is required" }),
  Schema.isMaxLength(255, {
    message: "Username must be 255 characters or fewer",
  }),
);
export const registryCredentialSecretSchema = Schema.String.check(
  Schema.isNonEmpty({ message: "Secret is required" }),
  Schema.isMaxLength(32_768, {
    message: "Secret must be 32768 characters or fewer",
  }),
);

export const serviceCommandSchema = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Command is required" }),
  Schema.isMaxLength(2000),
);
export const serviceNullableCommandSchema = Schema.NullOr(
  serviceCommandSchema,
);
export const serviceHealthcheckPathSchema = Schema.Trim.check(
  Schema.isNonEmpty({ message: "Healthcheck path is required" }),
  Schema.isMaxLength(500),
  Schema.isStartsWith("/", {
    message: "Healthcheck path must start with /",
  }),
);
export const serviceHealthcheckTimeoutSecondsSchema = Schema.Int.check(
  Schema.isBetween({ minimum: 1, maximum: 300 }),
);

const serviceGitBranchSchema = Schema.Union([
  Schema.Struct({
    type: Schema.Literal("connected"),
    name: serviceBranchNameSchema,
  }),
  Schema.Struct({
    type: Schema.Literal("disconnected"),
    previousName: Schema.NullOr(serviceBranchNameSchema),
  }),
]);

const serviceImageAutoUpdateSchema = Schema.Union([
  Schema.Struct({ type: Schema.Literal("off") }),
  Schema.Struct({
    type: Schema.Literal("track-tag"),
    tag: serviceTagSchema,
  }),
]);

const serviceImageCredentialsSchema = Schema.Union([
  Schema.Struct({ type: Schema.Literal("none") }),
  Schema.Struct({
    type: Schema.Literal("configured"),
    revision: Schema.optionalKey(Schema.String),
  }),
]);

export const serviceHealthcheckSchema = Schema.Union([
  Schema.Struct({ type: Schema.Literal("none") }),
  Schema.Struct({
    type: Schema.Literal("http"),
    path: serviceHealthcheckPathSchema,
    timeoutSeconds: serviceHealthcheckTimeoutSecondsSchema,
  }),
]);

export const valuePartSchema = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("text"), value: Schema.String }),
  Schema.Struct({
    kind: Schema.Literal("ref"),
    owner: Schema.Union([
      Schema.Struct({ scope: Schema.Literal("self") }),
      Schema.Struct({
        scope: Schema.Literals(["service", "variable_group"]),
        lineageId: Schema.String,
      }),
    ]),
    key: Schema.String,
  }),
]);

const serviceDeployEnvSourceSchema = Schema.Struct({
  kind: Schema.Literal("variable_group"),
  resourceId: Uuid,
  resourceName: Schema.NonEmptyString,
  variableGroupId: Uuid,
  key: Schema.NonEmptyString,
});

const serviceDeployEnvLiteralValueSchema = Schema.Struct({
  kind: Schema.Literal("literal"),
  value: Schema.String,
  source: Schema.optionalKey(serviceDeployEnvSourceSchema),
  parts: Schema.optionalKey(Schema.mutable(Schema.Array(valuePartSchema))),
});

const serviceDeployEnvSecretValueSchema = Schema.Struct({
  kind: Schema.Literal("secret"),
  variableId: Schema.optionalKey(Uuid),
  encryptedValue: Schema.optionalKey(encryptedSecretValueSchema),
  fingerprint: Schema.NonEmptyString,
  source: Schema.optionalKey(serviceDeployEnvSourceSchema),
});

const serviceDeployEnvValueSchema = Schema.Union([
  serviceDeployEnvLiteralValueSchema,
  serviceDeployEnvSecretValueSchema,
]);

const serviceDeployEnvSchema = Schema.Record(
  Schema.NonEmptyString,
  Schema.mutableKey(serviceDeployEnvValueSchema),
);

export const SERVICE_RESTART_POLICIES = [
  "unless-stopped",
  "always",
  "on-failure",
  "no",
] as const satisfies readonly ServiceRestartPolicyRecord[];

export const serviceRestartPolicySchema = Schema.Literals(
  SERVICE_RESTART_POLICIES,
);
export const serviceMaxRetriesSchema = Schema.Int.check(
  Schema.isBetween({ minimum: 0, maximum: 100 }),
);
export const serviceReplicasSchema = Schema.Int.check(
  Schema.isBetween({ minimum: 0, maximum: 50 }),
);
export const serviceCpuLimitSchema = Schema.NullOr(
  Schema.Finite.check(
    Schema.isGreaterThan(0),
    Schema.isLessThanOrEqualTo(64),
  ),
);
export const serviceMemLimitSchema = Schema.NullOr(
  Schema.Finite.check(
    Schema.isGreaterThan(0),
    Schema.isLessThanOrEqualTo(1024),
  ),
);
export const serviceCronSchema = Schema.NullOr(
  Schema.Trim.check(Schema.isNonEmpty()),
);
export const servicePrivateDnsSchema = Schema.Trim.check(
  Schema.isPattern(/^[A-Za-z0-9_-]+$/, {
    message: "Use only letters, numbers, underscores, or dashes.",
  }),
);

export const serviceRouteSchema = Schema.Struct({
  id: Uuid,
  hostname: Schema.Trim.check(Schema.isNonEmpty()),
  targetPort: Schema.Int.check(
    Schema.isBetween({ minimum: 1, maximum: 65_535 }),
  ),
});
export const serviceRoutesSchema = Schema.mutable(
  Schema.Array(serviceRouteSchema),
).check(
  Schema.makeFilter((routes) => {
    const seen = new Set<string>();
    const issues: Schema.FilterIssue[] = [];
    for (const [index, route] of routes.entries()) {
      if (seen.has(route.id)) {
        issues.push({ path: [index, "id"], issue: "Route IDs must be unique." });
      }
      seen.add(route.id);
    }
    return issues;
  }),
);

export const serviceManagedHostnamePrefixSchema = Schema.Trim.pipe(
  Schema.decode({
    decode: SchemaGetter.transform((value) => value.toLowerCase()),
    encode: SchemaGetter.transform((value) => value.toLowerCase()),
  }),
  Schema.check(
    Schema.isPattern(/^[a-z0-9]([a-z0-9-]*[a-z0-9])?$/, {
      message:
        "Use lowercase letters, numbers, or dashes (no leading/trailing dash).",
    }),
  ),
);
export const serviceManagedHostnameSchema = Schema.Struct({
  prefix: serviceManagedHostnamePrefixSchema,
  targetPort: Schema.NullOr(
    Schema.Int.check(
      Schema.isBetween({ minimum: 1, maximum: 65_535 }),
    ),
  ),
});

export const serviceBuilderSchema = Schema.Literals(["dockerfile", "auto"]);
export const serviceBuildConfigSchema = Schema.Struct({
  builder: serviceBuilderSchema,
  dockerfilePath: Schema.NullOr(
    Schema.Trim.check(Schema.isNonEmpty()),
  ),
  watchPaths: Schema.mutable(
    Schema.Array(Schema.Trim.check(Schema.isNonEmpty())),
  ),
});
export const DEFAULT_SERVICE_BUILD_CONFIG: ServiceBuildConfig = {
  builder: "auto",
  dockerfilePath: null,
  watchPaths: [],
};

export const serviceSourceSchema = Schema.Union([
  Schema.Struct({
    version: Schema.Literal(1),
    type: Schema.Literal("empty"),
    rootDir: serviceRootDirSchema,
  }),
  Schema.Struct({
    version: Schema.Literal(2),
    type: Schema.Literal("git"),
    repository: serviceRepositorySchema,
    repositoryId: Schema.Int.check(Schema.isGreaterThan(0)),
    installationId: Schema.Int.check(Schema.isGreaterThan(0)),
    rootDir: serviceRootDirSchema,
    branch: serviceGitBranchSchema,
    autoDeploy: Schema.Boolean,
    waitForCi: Schema.Boolean,
  }),
  Schema.Struct({
    version: Schema.Literal(1),
    type: Schema.Literal("image"),
    image: serviceImageSchema,
    autoUpdate: serviceImageAutoUpdateSchema,
    credentials: serviceImageCredentialsSchema,
  }),
]);

const serviceDeployMountSchema = Schema.Struct({
  volumeResourceId: Uuid,
  volumeName: Schema.NonEmptyString,
  mountPath: Schema.NonEmptyString,
});

export const serviceDeploymentConfigSchema = Schema.Struct({
  version: Schema.Literal(2),
  name: serviceName,
  source: serviceSourceSchema,
  preDeployCommand: serviceNullableCommandSchema,
  startCommand: serviceNullableCommandSchema,
  healthcheck: serviceHealthcheckSchema,
  restartPolicy: serviceRestartPolicySchema,
  maxRetries: serviceMaxRetriesSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(10)),
  ),
  cron: serviceCronSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(null)),
  ),
  replicas: serviceReplicasSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(1)),
  ),
  cpuLimit: serviceCpuLimitSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(null)),
  ),
  memLimit: serviceMemLimitSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(null)),
  ),
  privateDns: servicePrivateDnsSchema,
  routes: serviceRoutesSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed([])),
  ),
  managedHostname: Schema.NullOr(serviceManagedHostnameSchema).pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(null)),
  ),
  build: serviceBuildConfigSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(DEFAULT_SERVICE_BUILD_CONFIG)),
  ),
  env: serviceDeployEnvSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed({})),
  ),
  mounts: Schema.mutable(Schema.Array(serviceDeployMountSchema)).pipe(
    Schema.withDecodingDefaultKey(Effect.succeed([])),
  ),
});

export type ServiceDeployMount = DeepMutable<
  typeof serviceDeployMountSchema.Type
>;

const serviceDbSelectSchema = createSelectSchema(service, {
  name: serviceName,
  slug: requiredTrimmedString,
  sourceType: Schema.Literals(SERVICE_SOURCE_TYPES),
  sourceConfig: serviceSourceSchema,
  preDeployCommand: serviceNullableCommandSchema,
  startCommand: serviceNullableCommandSchema,
  healthcheck: serviceHealthcheckSchema,
  restartPolicy: serviceRestartPolicySchema,
  maxRetries: serviceMaxRetriesSchema,
  cron: serviceCronSchema,
  replicas: serviceReplicasSchema,
  cpuLimit: serviceCpuLimitSchema,
  memLimit: serviceMemLimitSchema,
  privateDns: servicePrivateDnsSchema,
  routes: serviceRoutesSchema,
  managedHostname: Schema.NullOr(serviceManagedHostnameSchema),
  build: serviceBuildConfigSchema,
});

export const serviceInsertSchema = createInsertSchema(service, {
  name: serviceName,
  slug: requiredTrimmedString,
  sourceType: Schema.Literals(SERVICE_SOURCE_TYPES),
  sourceConfig: serviceSourceSchema,
  preDeployCommand: serviceNullableCommandSchema,
  startCommand: serviceNullableCommandSchema,
  healthcheck: serviceHealthcheckSchema,
  restartPolicy: serviceRestartPolicySchema,
});

export const serviceSelectSchema = Schema.Struct({
  id: serviceDbSelectSchema.fields.id,
  environmentId: serviceDbSelectSchema.fields.environmentId,
  lineageId: serviceDbSelectSchema.fields.lineageId,
  name: serviceDbSelectSchema.fields.name,
  slug: serviceDbSelectSchema.fields.slug,
  source: serviceSourceSchema,
  registryCredentialUsername: Schema.NullOr(registryCredentialUsernameSchema),
  hasStoredRegistryCredential: Schema.Boolean,
  preDeployCommand: serviceDbSelectSchema.fields.preDeployCommand,
  startCommand: serviceDbSelectSchema.fields.startCommand,
  healthcheck: serviceDbSelectSchema.fields.healthcheck,
  restartPolicy: serviceDbSelectSchema.fields.restartPolicy,
  maxRetries: serviceDbSelectSchema.fields.maxRetries,
  cron: serviceDbSelectSchema.fields.cron,
  replicas: serviceDbSelectSchema.fields.replicas,
  cpuLimit: serviceDbSelectSchema.fields.cpuLimit,
  memLimit: serviceDbSelectSchema.fields.memLimit,
  privateDns: serviceDbSelectSchema.fields.privateDns,
  routes: serviceDbSelectSchema.fields.routes,
  managedHostname: serviceDbSelectSchema.fields.managedHostname,
  build: serviceDbSelectSchema.fields.build,
  firstDeployedAt: serviceDbSelectSchema.fields.firstDeployedAt,
  deletedAt: serviceDbSelectSchema.fields.deletedAt,
  createdAt: serviceDbSelectSchema.fields.createdAt,
  updatedAt: serviceDbSelectSchema.fields.updatedAt,
});

export const serviceWithContextSelectSchema = Schema.Struct({
  ...serviceSelectSchema.fields,
  projectSlug: ProjectSlug,
  environmentSlug: EnvironmentSlug,
});

const canvasPositionDbSelectSchema = createSelectSchema(
  environmentCanvasNodePosition,
);
export const serviceCanvasPositionSelectSchema = Schema.Struct({
  id: canvasPositionDbSelectSchema.fields.id,
  environmentId: canvasPositionDbSelectSchema.fields.environmentId,
  resourceType: canvasPositionDbSelectSchema.fields.resourceType,
  resourceId: canvasPositionDbSelectSchema.fields.resourceId,
  x: canvasPositionDbSelectSchema.fields.x,
  y: canvasPositionDbSelectSchema.fields.y,
  createdAt: canvasPositionDbSelectSchema.fields.createdAt,
  updatedAt: canvasPositionDbSelectSchema.fields.updatedAt,
});

export const savedServiceIntentConfigEffectSchema = Schema.Struct({
  version: serviceDeploymentConfigSchema.fields.version,
  name: serviceDeploymentConfigSchema.fields.name,
  source: serviceDeploymentConfigSchema.fields.source,
  preDeployCommand: serviceDeploymentConfigSchema.fields.preDeployCommand,
  startCommand: serviceDeploymentConfigSchema.fields.startCommand,
  healthcheck: serviceDeploymentConfigSchema.fields.healthcheck,
  restartPolicy: serviceDeploymentConfigSchema.fields.restartPolicy,
  maxRetries: serviceDeploymentConfigSchema.fields.maxRetries,
  cron: serviceDeploymentConfigSchema.fields.cron,
  replicas: serviceDeploymentConfigSchema.fields.replicas,
  cpuLimit: serviceDeploymentConfigSchema.fields.cpuLimit,
  memLimit: serviceDeploymentConfigSchema.fields.memLimit,
  privateDns: serviceDeploymentConfigSchema.fields.privateDns,
  routes: serviceDeploymentConfigSchema.fields.routes,
  managedHostname: serviceDeploymentConfigSchema.fields.managedHostname,
  build: serviceDeploymentConfigSchema.fields.build,
});

const serviceRegistryCredentialScopeFields = {
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  serviceId: Uuid,
};

export const deleteServicesSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  serviceIds: Schema.Array(Uuid).check(Schema.isMinLength(1)),
});

export const updateServiceCanvasPositionSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  serviceId: Uuid,
  x: Schema.Finite,
  y: Schema.Finite,
});

export const setServiceRegistryCredentialSchema = Schema.Struct({
  ...serviceRegistryCredentialScopeFields,
  username: Schema.optional(Schema.NullOr(registryCredentialUsernameSchema)),
  secret: registryCredentialSecretSchema,
});
export const clearServiceRegistryCredentialSchema = Schema.Struct(
  serviceRegistryCredentialScopeFields,
);
export const restoreServiceRegistryCredentialSchema =
  clearServiceRegistryCredentialSchema;

export const createServiceSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  x: Schema.Finite,
  y: Schema.Finite,
  name: Schema.optionalKey(serviceSelectSchema.fields.name),
  source: serviceSourceSchema,
  preDeployCommand: serviceNullableCommandSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(null)),
  ),
  startCommand: serviceNullableCommandSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed(null)),
  ),
  healthcheck: serviceHealthcheckSchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed({ type: "none" })),
  ),
  restartPolicy: serviceRestartPolicySchema.pipe(
    Schema.withDecodingDefaultKey(Effect.succeed("unless-stopped")),
  ),
});

export const updateServiceSchema = Schema.Struct({
  ...serviceRegistryCredentialScopeFields,
  name: Schema.optionalKey(serviceSelectSchema.fields.name),
  source: Schema.optionalKey(serviceSourceSchema),
  preDeployCommand: Schema.optionalKey(
    serviceSelectSchema.fields.preDeployCommand,
  ),
  startCommand: Schema.optionalKey(serviceSelectSchema.fields.startCommand),
  healthcheck: Schema.optionalKey(serviceSelectSchema.fields.healthcheck),
  restartPolicy: Schema.optionalKey(serviceSelectSchema.fields.restartPolicy),
  maxRetries: Schema.optionalKey(serviceSelectSchema.fields.maxRetries),
  cron: Schema.optionalKey(serviceSelectSchema.fields.cron),
  replicas: Schema.optionalKey(serviceSelectSchema.fields.replicas),
  cpuLimit: Schema.optionalKey(serviceSelectSchema.fields.cpuLimit),
  memLimit: Schema.optionalKey(serviceSelectSchema.fields.memLimit),
  privateDns: Schema.optionalKey(serviceSelectSchema.fields.privateDns),
  routes: Schema.optionalKey(serviceSelectSchema.fields.routes),
  managedHostname: Schema.optionalKey(
    serviceSelectSchema.fields.managedHostname,
  ),
  build: Schema.optionalKey(serviceSelectSchema.fields.build),
  deletedAt: Schema.optionalKey(serviceSelectSchema.fields.deletedAt),
});

export const restoreServiceWorkingIntentSchema = Schema.Struct({
  ...serviceRegistryCredentialScopeFields,
  savedStateSnapshotId: Uuid,
});

export type ServiceSource = DeepMutable<typeof serviceSourceSchema.Type>;
export type ServiceGitBranch = DeepMutable<typeof serviceGitBranchSchema.Type>;
export type ServiceImageAutoUpdate = DeepMutable<
  typeof serviceImageAutoUpdateSchema.Type
>;
export type ServiceImageCredentials = DeepMutable<
  typeof serviceImageCredentialsSchema.Type
>;
export type RegistryCredentialProvider =
  typeof registryCredentialProviderSchema.Type;
export type RegistryCredentialAuthMode =
  typeof registryCredentialAuthModeSchema.Type;
export type ServiceHealthcheck = DeepMutable<
  typeof serviceHealthcheckSchema.Type
>;
export type ServiceRestartPolicy = typeof serviceRestartPolicySchema.Type;
export type ServiceDeployEnvValue = DeepMutable<
  typeof serviceDeployEnvValueSchema.Type
>;
export type ServiceDeployEnv = DeepMutable<typeof serviceDeployEnvSchema.Type>;
export type ServiceDeploymentConfig = DeepMutable<
  typeof serviceDeploymentConfigSchema.Type
>;
export type ServiceRecord = DeepMutable<typeof serviceSelectSchema.Type>;
export type ServiceWithContextRecord = DeepMutable<
  typeof serviceWithContextSelectSchema.Type
>;
export type ServiceCanvasPositionRecord = DeepMutable<
  typeof serviceCanvasPositionSelectSchema.Type
>;
export type CreateServiceInput = typeof createServiceSchema.Type;
export type UpdateServiceInput = typeof updateServiceSchema.Type;
export type RestoreServiceWorkingIntentInput =
  typeof restoreServiceWorkingIntentSchema.Type;
export type SetServiceRegistryCredentialInput =
  typeof setServiceRegistryCredentialSchema.Type;
export type ClearServiceRegistryCredentialInput =
  typeof clearServiceRegistryCredentialSchema.Type;
export type RestoreServiceRegistryCredentialInput =
  typeof restoreServiceRegistryCredentialSchema.Type;
