/** A Server as `ployz device enforce` reaches it: over its own SSH, at the address Cloud last saw. */
export type EnforceTarget = { readonly name: string; readonly publicIp: string | null };

/**
 * `ployz device enforce` with one `--connect` per Server, as a Cluster enrolled through Ployz Cloud needs: no computer
 * saved its SSH connections, and a Server holding keys from before takes the set only over its own SSH. A Server with
 * no known address shows its name to fill in.
 */
export function enforceCommand(servers: readonly EnforceTarget[]): string {
  return overSsh("ployz device enforce", servers);
}

/** `ployz device approve --this-device` over each Server's own SSH: a first key that trusts this computer and the Server's SSH, not Ployz Cloud. */
export function thisDeviceCommand(servers: readonly EnforceTarget[]): string {
  return overSsh("ployz device approve --this-device", servers);
}

function overSsh(command: string, servers: readonly EnforceTarget[]): string {
  return [command, ...servers.map(({ name, publicIp }) => `  --connect ssh://root@${publicIp ?? `<${name}>`}`)].join(" \\\n");
}
