import "@tanstack/react-start/server-only";
import { createHash } from "node:crypto";
import { managedHostname } from "#/modules/environment-design/managed-service-exports";
import type { ServiceDeploymentConfig } from "#/modules/environment-design/services";

/** Managed hostnames reach the daemon as explicit `prefix.name` routes, never as bare prefixes. */
export function expandManagedHostnames(config: ServiceDeploymentConfig, clusterDomain: string): ServiceDeploymentConfig {
  if (config.managedHostnames.length === 0) return config;
  return {
    ...config,
    routes: [...config.routes, ...config.managedHostnames.map(({ prefix, targetPort }) => {
      const hostname = managedHostname(prefix, clusterDomain);
      return { id: hostnameRouteId(hostname), hostname, targetPort };
    })],
    managedHostnames: [],
  };
}

/** A route id that is stable per hostname: an RFC 4122 v5-shaped UUID over its SHA-1, as core route validation requires a UUID. */
function hostnameRouteId(hostname: string) {
  const bytes = createHash("sha1").update(`ployz.managed-hostname:${hostname}`).digest().subarray(0, 16);
  bytes.writeUInt8((bytes.readUInt8(6) & 0x0f) | 0x50, 6);
  bytes.writeUInt8((bytes.readUInt8(8) & 0x3f) | 0x80, 8);
  const hex = bytes.toString("hex");
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
