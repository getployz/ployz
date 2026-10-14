export const CONNECT_NOTICES = ["linked", "none", "not-yours", "not-admin", "pending", "failed"] as const;
export type ConnectNotice = typeof CONNECT_NOTICES[number];
