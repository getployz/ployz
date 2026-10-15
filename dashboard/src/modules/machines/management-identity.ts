const CAPABILITY = /^ployz1:([A-Za-z0-9_-]{86})$/;

/** The management public key a `ployz1:` capability carries, as 64 hex digits; null when it isn't one. */
export function managementIdentity(capability: string): string | null {
  const body = CAPABILITY.exec(capability)?.[1];
  return body === undefined ? null : Buffer.from(body, "base64url").subarray(0, 32).toString("hex");
}
