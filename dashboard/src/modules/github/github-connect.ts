export const CONNECT_NOTICES = ["linked", "none", "not-yours", "not-admin", "pending", "failed"] as const;
export type ConnectNotice = typeof CONNECT_NOTICES[number];
/**
 * The most accounts one connect offers and one accept takes. The signed offer rides in the settings URL,
 * and this many ids at the largest safe integer still fit the 8192-character offer bound.
 */
export const MAX_OFFERED_INSTALLATIONS = 300;
