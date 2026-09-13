import {
  createInsertSchema,
  createSelectSchema,
} from "drizzle-orm/effect-schema";
import { Effect, Schema } from "effect";
import { serviceFieldSchema, sharedServiceConfigSchema, savedServiceConfigSchema } from "./service-config";
import type { ServiceConfig, ServiceGitBranch as SharedServiceGitBranch, ServiceImageAutoUpdate as SharedServiceImageAutoUpdate, ServiceImageCredentials as SharedServiceImageCredentials } from "@ployz/sdk/config";
import {
  environmentCanvasNodePosition,
  REGISTRY_CREDENTIAL_AUTH_MODES,
  REGISTRY_CREDENTIAL_PROVIDERS,
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
const serviceName = serviceFieldSchema("name");
export const serviceRootDirSchema = serviceFieldSchema("rootDir");

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

export const serviceCommandSchema = serviceFieldSchema("command");
export const serviceNullableCommandSchema = serviceFieldSchema("startCommand");
export const serviceHealthcheckPathSchema = serviceFieldSchema("healthcheckPath");
export const serviceHealthcheckTimeoutSecondsSchema = serviceFieldSchema("healthcheckTimeoutSeconds");
export const serviceHealthcheckSchema = serviceFieldSchema("healthcheck");

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

export const serviceRestartPolicySchema = serviceFieldSchema("restartPolicy");
export const serviceMaxRetriesSchema = serviceFieldSchema("maxRetries");
export const serviceReplicasSchema = serviceFieldSchema("replicas");
export const serviceCpuLimitSchema = serviceFieldSchema("cpuLimit");
export const serviceMemLimitSchema = serviceFieldSchema("memLimit");
export const serviceCronSchema = serviceFieldSchema("cron");
export const servicePrivateDnsSchema = serviceFieldSchema("privateDns");
export const serviceRouteSchema = Schema.Struct({
  id: Uuid, hostname: Schema.String, targetPort: Schema.Number,
});
export const serviceRoutesSchema = serviceFieldSchema("routes");
export const serviceManagedHostnamePrefixSchema = serviceFieldSchema("managedHostnamePrefix");
export const serviceManagedHostnameSchema = serviceFieldSchema("managedHostnameValue");
export const serviceBuilderSchema = Schema.Literals(["dockerfile", "auto"]);
export const serviceBuildConfigSchema = serviceFieldSchema("build");
export const DEFAULT_SERVICE_BUILD_CONFIG: ServiceBuildConfig = {
  builder: "auto", dockerfilePath: null, watchPaths: [],
};
export const serviceSourceSchema = serviceFieldSchema("source");

const serviceDeployMountSchema = Schema.Struct({
  volumeResourceId: Uuid,
  volumeName: Schema.NonEmptyString,
  mountPath: Schema.NonEmptyString,
});

export const serviceDeploymentConfigSchema = sharedServiceConfigSchema;

export type ServiceDeployMount = DeepMutable<
  typeof serviceDeployMountSchema.Type
>;

const serviceDbSelectSchema = createSelectSchema(service);
export const serviceInsertSchema = createInsertSchema(service);

/** Read-only presentation of a service in the authored Environment document. */
export const serviceSelectSchema = Schema.Struct({
  id: Uuid,
  environmentId: Uuid,
  lineageId: Uuid,
  name: serviceName,
  slug: requiredTrimmedString,
  source: serviceSourceSchema,
  registryCredentialUsername: Schema.NullOr(registryCredentialUsernameSchema),
  hasStoredRegistryCredential: Schema.Boolean,
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
  firstDeployedAt: serviceDbSelectSchema.fields.firstDeployedAt,
  deletedAt: Schema.NullOr(Schema.Date),
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

export const savedServiceIntentConfigEffectSchema = savedServiceConfigSchema;

const serviceRegistryCredentialScopeFields = {
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  serviceId: Uuid,
  revision: Uuid,
};

export const deleteServicesSchema = Schema.Struct({
  organizationSlug: OrganizationSlug,
  environmentId: Uuid,
  revision: Uuid,
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

export type ServiceSource = DeepMutable<typeof serviceSourceSchema.Type>;
export type ServiceGitBranch = SharedServiceGitBranch;
export type ServiceImageAutoUpdate = SharedServiceImageAutoUpdate;
export type ServiceImageCredentials = SharedServiceImageCredentials;
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
export type ServiceDeploymentConfig = ServiceConfig;
export type ServiceRecord = DeepMutable<typeof serviceSelectSchema.Type>;
export type ServiceWithContextRecord = DeepMutable<
  typeof serviceWithContextSelectSchema.Type
>;
export type ServiceCanvasPositionRecord = DeepMutable<
  typeof serviceCanvasPositionSelectSchema.Type
>;
export type CreateServiceInput = typeof createServiceSchema.Type;
export type UpdateServiceInput = typeof updateServiceSchema.Type;
export type SetServiceRegistryCredentialInput =
  typeof setServiceRegistryCredentialSchema.Type;
export type ClearServiceRegistryCredentialInput =
  typeof clearServiceRegistryCredentialSchema.Type;
export type RestoreServiceRegistryCredentialInput =
  typeof restoreServiceRegistryCredentialSchema.Type;
