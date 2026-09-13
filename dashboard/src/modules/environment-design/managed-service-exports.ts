import type { ServiceWithContextRecord } from "#/modules/environment-design/services";

const PLATFORM_HTTP_PORT = 3000;

type ServiceExportContext = Pick<
  ServiceWithContextRecord,
  "id" | "lineageId" | "name" | "slug" | "environmentId" | "environmentSlug"
>;

export interface ManagedServiceExportRecord {
  readonly serviceId: string;
  // Stable cross-environment identity of the owning service, used to build
  // rename-safe `${{ }}` references in the autocomplete.
  readonly serviceLineageId: string;
  readonly serviceName: string;
  readonly serviceSlug: string;
  readonly key: string;
  readonly description: string;
  readonly value: string;
  readonly exported: true;
  readonly managed: true;
}

export function getManagedServiceExports(
  service: ServiceExportContext,
): ManagedServiceExportRecord[] {
  const definitions: Array<Pick<
    ManagedServiceExportRecord,
    "key" | "description" | "value"
  >> = [
    {
      key: "PLOYZ_PRIVATE_DOMAIN",
      description: "The private DNS name of the service.",
      value: `${service.slug}-${service.environmentSlug}.internal`,
    },
    {
      key: "PORT",
      description: "The platform HTTP port exposed by the service.",
      value: String(PLATFORM_HTTP_PORT),
    },
    {
      key: "PLOYZ_ENVIRONMENT_NAME",
      description: "The environment name of the service instance.",
      value: service.environmentSlug,
    },
    {
      key: "PLOYZ_SERVICE_NAME",
      description: "The service name.",
      value: service.name,
    },
    {
      key: "PLOYZ_ENVIRONMENT_ID",
      description: "The environment ID of the service instance.",
      value: service.environmentId,
    },
    {
      key: "PLOYZ_SERVICE_ID",
      description: "The service ID.",
      value: service.id,
    },
  ];

  return definitions.map((definition) => ({
    serviceId: service.id,
    serviceLineageId: service.lineageId,
    serviceName: service.name,
    serviceSlug: service.slug,
    exported: true,
    managed: true,
    ...definition,
  }));
}
