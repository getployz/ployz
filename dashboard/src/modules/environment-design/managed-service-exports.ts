import type { ServiceWithContextRecord } from "#/modules/environment-design/services";

const PLATFORM_HTTP_PORT = 3000;

type ServiceExportContext = Pick<
  ServiceWithContextRecord,
  "id" | "lineageId" | "name" | "slug" | "environmentId" | "environmentSlug" | "privateDns" | "routes" | "managedHostnames"
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

/** A managed hostname expanded against the Organization's Cluster Domain. */
export const managedHostname = (prefix: string, clusterDomain: string) => `${prefix}.${clusterDomain}`;

/** Domain lists preserve link order; editing a port does not change priority. */
export function servicePublicDomain(
  service: Pick<ServiceExportContext, "routes" | "managedHostnames">,
  clusterDomain: string | null,
): string | null {
  const custom = service.routes.at(-1);
  if (custom) return custom.hostname;
  const managed = service.managedHostnames.at(-1);
  return managed && clusterDomain ? managedHostname(managed.prefix, clusterDomain) : null;
}

export function getManagedServiceExports(
  service: ServiceExportContext,
  clusterDomain: string | null = null,
): ManagedServiceExportRecord[] {
  const definitions: Array<Pick<
    ManagedServiceExportRecord,
    "key" | "description" | "value"
  >> = [
    {
      key: "PLOYZ_PRIVATE_DOMAIN",
      description: "The private DNS name of the service.",
      value: `${service.privateDns}.internal`,
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
      description: "The stable service slug.",
      value: service.slug,
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

  const publicDomain = servicePublicDomain(service, clusterDomain);
  if (publicDomain) definitions.push({
    key: "PLOYZ_PUBLIC_DOMAIN",
    description: "The most recently linked custom domain, otherwise the most recently linked generated domain.",
    value: publicDomain,
  });

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
