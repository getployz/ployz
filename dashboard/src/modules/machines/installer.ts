import { PLOYZ_ORIGIN } from "#/lib/docs";

/** Where Ployz is installed from, and where the daemon reads its Release Channel pointers (core `RELEASE_CHANNEL_URL`). */
export const INSTALL_ORIGIN = "https://ployz.sh";

/** The line that installs or updates Ployz on the machine that runs it. */
export const INSTALL_COMMAND = `curl -fsSL ${INSTALL_ORIGIN} | sh`;

/** What a Server on an older management protocol runs to upgrade, from a computer that reaches it over SSH. */
export const SSH_UPGRADE = `${INSTALL_COMMAND}\nployz server upgrade stable SERVER --connect ssh://USER@HOST`;

/** The variable `ployz server add` reads its enrollment token from; `sudo --preserve-env` keeps it. */
export const ENROLL_TOKEN_ENV = "PLOYZ_ENROLL_TOKEN";

export type InstallLines = { readonly shell: string; readonly cloudInit: string };

export type InstallTarget = {
  readonly cloudOrigin: string;
  readonly ployzVersion: string;
  /** A single-use enrollment token for one Server. */
  readonly token: string;
  /** The first Server founds the Cluster with ports 80 and 443 closed, for Cloudflare Tunnel. */
  readonly ingressClosed: boolean;
};

/**
 * The line `ployz server add --command` prints, pinned to the release this Cloud's SDK speaks, since the stable channel
 * may be older or newer. The Shell line carries no token: `ployz server add` asks for it on the terminal, so it stays
 * out of shell history and `ps`. cloud-init has no terminal, so it exports the token first. `key` is the passkey the new
 * Server trusts to found or approve its join; it is public, so both lines carry it as it is.
 */
export function installLines({ cloudOrigin, ployzVersion, token, ingressClosed }: InstallTarget, key: string): InstallLines {
  const cloud = cloudOrigin === PLOYZ_ORIGIN ? "" : ` --cloud-url '${cloudOrigin}'`;
  const keyed = ` --key ${key}`;
  const closed = ingressClosed ? " --ingress-closed" : "";
  const shell = `curl -fsSL ${INSTALL_ORIGIN} | sh -s -- ${ployzVersion} && sudo --preserve-env=${ENROLL_TOKEN_ENV} ployz server add${cloud}${keyed}${closed}`;
  return {
    shell,
    cloudInit: `#cloud-config\nruncmd:\n  - export ${ENROLL_TOKEN_ENV}=${token}; ${shell}`,
  };
}
