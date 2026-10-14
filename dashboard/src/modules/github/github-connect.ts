export const CONNECT_NOTICES = ["linked", "none", "not-yours", "pending", "failed"] as const;
export type ConnectNotice = typeof CONNECT_NOTICES[number];
