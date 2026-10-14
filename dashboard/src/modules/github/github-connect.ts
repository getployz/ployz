/** What Organization › GitHub tells the person after GitHub sent them back from a connect. */
export const CONNECT_NOTICES = ["linked", "none", "not-yours", "pending", "failed"] as const;
export type ConnectNotice = typeof CONNECT_NOTICES[number];
