import { eventType, staticSchema } from "inngest";

export const destructiveVolumeRequestedEventName =
  "cloud/destructive-volume.requested" as const;

export const destructiveVolumeRequestedEventType = eventType(
  destructiveVolumeRequestedEventName,
  { schema: staticSchema<{ attemptId: string }>() },
);

export function destructiveVolumeRequestedEvent(attemptId: string) {
  return {
    id: `cloud-destructive-volume-attempt-${attemptId}`,
    name: destructiveVolumeRequestedEventName,
    data: { attemptId },
  } as const;
}
