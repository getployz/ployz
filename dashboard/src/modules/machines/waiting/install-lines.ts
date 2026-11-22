import type { FleetTokenLifetime } from "#/modules/machines/waiting/fleet-token";

const DEFAULT_CLOUD_ORIGIN = "https://ployz.dev";

export type InstallLines = { readonly shell: string; readonly cloudInit: string };

export type InstallTarget = { readonly cloudOrigin: string; readonly ployzVersion: string; readonly token: string };

/**
 * Every line pins the release this Cloud's SDK speaks, since the stable channel may be older or newer. The Shell line
 * carries no token: install.sh asks for it on the terminal, so it stays out of shell history and `ps`. cloud-init has no
 * terminal, so it passes the token in PLOYZ_FLEET_TOKEN. `key` is the passkey the new Server trusts to found or join
 * the Cluster; it is public, so both lines carry it as it is.
 */
export function installLines({ cloudOrigin, ployzVersion, token }: InstallTarget, key: string | null): InstallLines {
  const cloud = cloudOrigin === DEFAULT_CLOUD_ORIGIN ? "" : `PLOYZ_CLOUD_URL='${cloudOrigin}' `;
  const keyed = key === null ? "" : ` --key ${key}`;
  return {
    shell: `curl -fsSL https://ployz.sh | sudo ${cloud}sh -s -- ${ployzVersion} --enroll${keyed}`,
    cloudInit: `#cloud-config\nruncmd:\n  - curl -fsSL https://ployz.sh | ${cloud}PLOYZ_FLEET_TOKEN=${token} sh -s -- ${ployzVersion}${keyed}`,
  };
}

export function cliLifetimeLabel(expiresIn: FleetTokenLifetime): string {
  return expiresIn === "1d" ? "24h" : expiresIn;
}
