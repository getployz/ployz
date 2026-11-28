import { PLOYZ_ORIGIN } from "#/lib/docs";
import type { FleetTokenLifetime } from "#/modules/machines/waiting/fleet-token";


/** Where Ployz is installed from, and where the daemon reads its Release Channel pointers (core `RELEASE_CHANNEL_URL`). */
export const INSTALL_ORIGIN = "https://ployz.sh";

/** The line that installs or updates Ployz on the machine that runs it. */
export const INSTALL_COMMAND = `curl -fsSL ${INSTALL_ORIGIN} | sh`;

/** What a Server on an older management protocol runs to upgrade, from a computer that reaches it over SSH. */
export const SSH_UPGRADE = `${INSTALL_COMMAND}\nployz server upgrade stable SERVER --connect ssh://USER@HOST`;

export type InstallLines = { readonly shell: string; readonly cloudInit: string };

export type InstallTarget = { readonly cloudOrigin: string; readonly ployzVersion: string; readonly token: string };

/**
 * Every line pins the release this Cloud's SDK speaks, since the stable channel may be older or newer. The Shell line
 * carries no token: install.sh asks for it on the terminal, so it stays out of shell history and `ps`. cloud-init has no
 * terminal, so it passes the token in PLOYZ_FLEET_TOKEN. `key` is the passkey the new Server trusts to found or join
 * the Cluster; it is public, so both lines carry it as it is.
 */
export function installLines({ cloudOrigin, ployzVersion, token }: InstallTarget, key: string): InstallLines {
  const cloud = cloudOrigin === PLOYZ_ORIGIN ? "" : `PLOYZ_CLOUD_URL='${cloudOrigin}' `;
  const keyed = ` --key ${key}`;
  return {
    shell: `curl -fsSL ${INSTALL_ORIGIN} | sudo ${cloud}sh -s -- ${ployzVersion} --enroll${keyed}`,
    cloudInit: `#cloud-config\nruncmd:\n  - curl -fsSL ${INSTALL_ORIGIN} | ${cloud}PLOYZ_FLEET_TOKEN=${token} sh -s -- ${ployzVersion}${keyed}`,
  };
}

export function cliLifetimeLabel(expiresIn: FleetTokenLifetime): string {
  return expiresIn === "1d" ? "24h" : expiresIn;
}
