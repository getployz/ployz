/** The emails Cloud sends. Each has one preference row per user, Email unless they chose None. */
export const NOTIFICATION_TYPES = {
  uptime: {
    label: "Uptime incidents",
    description: "When a check goes down, and when it recovers.",
    unsubscribe: "Turn off uptime emails",
  },
  "deploy-failed": {
    label: "Failed GitHub deploys",
    description: "When a deploy from a push or pull request fails.",
    unsubscribe: "Turn off failed deploy emails",
  },
  "upgrade-failed": {
    label: "Failed automatic upgrades",
    description: "When an automatic Server upgrade fails.",
    unsubscribe: "Turn off failed upgrade emails",
  },
} as const;

export type NotificationType = keyof typeof NOTIFICATION_TYPES;

export const isNotificationType = (value: string): value is NotificationType => Object.hasOwn(NOTIFICATION_TYPES, value);

export const NOTIFICATION_TYPE_IDS = Object.keys(NOTIFICATION_TYPES).filter(isNotificationType);

export const NOTIFICATION_CHANNELS = ["email", "none"] as const;

export type NotificationChannel = (typeof NOTIFICATION_CHANNELS)[number];

/** No stored preference means Email. */
export const DEFAULT_NOTIFICATION_CHANNEL: NotificationChannel = "email";
