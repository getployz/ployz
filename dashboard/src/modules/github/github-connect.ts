export const CONNECT_NOTICES = ["linked", "none", "not-yours", "not-admin", "pending", "failed"] as const;
export type ConnectNotice = typeof CONNECT_NOTICES[number];
/**
 * The most accounts one connect offers and one accept takes. The signed offer rides in the settings URL,
 * and this many ids at the largest safe integer still fit the `OFFER_MAX_LENGTH` bound.
 */
export const MAX_OFFERED_INSTALLATIONS = 300;

/** The longest signed offer the settings URL and the accept call take. */
export const OFFER_MAX_LENGTH = 8192;

/** Starts a connect. `existing` links installations the user already has instead of installing the app again. */
export function githubConnectHref(organizationSlug: string, existing: boolean) {
  const params = new URLSearchParams({ organization: organizationSlug });
  if (existing) params.set("existing", "1");
  return `/api/github/connect?${params.toString()}`;
}
