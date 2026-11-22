
/** The PORT Ployz gives every Service that sets none of its own; Core's DEFAULT_SERVICE_PORT. */
export const PLATFORM_HTTP_PORT = 8080;

type ServiceExportContext = {
  id: string;
  environmentId: string;
  environmentSlug: string;
  privateDns: string;
};

type ManagedServiceExport = { readonly key: string; readonly description: string; readonly value: string };

export function getManagedServiceExports(service: ServiceExportContext): ManagedServiceExport[] {
  return [
    {
      key: "PLOYZ_PRIVATE_DOMAIN",
      description: "The private DNS name of the service.",
      value: `${service.privateDns}.internal`,
    },
    {
      key: "PORT",
      description: "The default HTTP port when PORT is unset.",
      value: String(PLATFORM_HTTP_PORT),
    },
    {
      key: "PLOYZ_ENVIRONMENT_NAME",
      description: "The environment name of the service instance.",
      value: service.environmentSlug,
    },
    {
      key: "PLOYZ_SERVICE_NAME",
      description: "The service's stable private DNS name.",
      value: service.privateDns,
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
}
