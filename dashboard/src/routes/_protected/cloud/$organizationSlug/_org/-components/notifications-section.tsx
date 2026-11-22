import { Select, SelectContent, SelectGroup, SelectItem, SelectTrigger, SelectValue } from "#/components/ui/select";
import {
  NOTIFICATION_CHANNELS,
  NOTIFICATION_TYPE_IDS,
  NOTIFICATION_TYPES,
  type NotificationChannel,
  type NotificationSettings,
  type SetNotificationChannel,
} from "#/modules/notifications/notification-types";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { docsUrl } from "#/lib/docs";

export const EMAIL_SETUP_DOCS = docsUrl("/docs/self-hosting#2-configure");
export const LOG_SHIPPING_DOCS = docsUrl("/docs/self-hosting#keep-logs-metrics-and-traces-in-cloud-optional");

const LINK = "underline underline-offset-4 hover:text-foreground";

const CHANNEL_LABELS = { email: "Email", none: "None" } satisfies Record<NotificationChannel, string>;

/** One row per email type. With no Mailer the rows still show what each type would do, and can't be changed. */
export function NotificationsSection({ settings, onChange }: {
  settings: NotificationSettings;
  onChange: (change: SetNotificationChannel) => void;
}) {
  return (
    <SettingsSection
      id="notifications"
      title="Notifications"
      description={settings.mailOn ? undefined : (
        <>Email isn't set up on this Cloud. <a href={EMAIL_SETUP_DOCS} target="_blank" rel="noreferrer" className={LINK}>Set up email</a></>
      )}
    >
      {NOTIFICATION_TYPE_IDS.map((type) => {
        const channel = settings.channels[type];
        const { label, description } = NOTIFICATION_TYPES[type];
        const needsShipping = type === "crash" && !settings.shippingOn;
        return (
          <SettingsRow
            key={type}
            label={label}
            hint={needsShipping ? (
              <>Crash emails need Cloud log shipping. <a href={LOG_SHIPPING_DOCS} target="_blank" rel="noreferrer" className={LINK}>Set up log shipping</a></>
            ) : description}
            commit="change"
            control={(
              <Select
                value={channel}
                disabled={!settings.mailOn}
                onValueChange={(next) => {
                  const chosen = NOTIFICATION_CHANNELS.find((candidate) => candidate === next);
                  if (chosen && chosen !== channel) onChange({ type, channel: chosen });
                }}
              >
                <SelectTrigger aria-label={label}>
                  <SelectValue>{CHANNEL_LABELS[channel]}</SelectValue>
                </SelectTrigger>
                <SelectContent>
                  <SelectGroup>
                    {NOTIFICATION_CHANNELS.map((option) => (
                      <SelectItem key={option} value={option} label={CHANNEL_LABELS[option]}>{CHANNEL_LABELS[option]}</SelectItem>
                    ))}
                  </SelectGroup>
                </SelectContent>
              </Select>
            )}
          />
        );
      })}
    </SettingsSection>
  );
}
