import { queryOptions, useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import type { NotificationSettings, SetNotificationChannel } from "#/modules/notifications/notification-types";
import { getNotificationSettingsServerFn, setNotificationChannelServerFn } from "#/modules/notifications/notifications.functions";

const settingsKey = ["notification-settings"] as const;

/** The signed-in user's email choices. Only this user changes them, and each change refetches. */
export const notificationSettingsQueryOptions = () =>
  queryOptions({
    queryKey: settingsKey,
    queryFn: () => getNotificationSettingsServerFn(),
    staleTime: 60_000,
  });

export function useSetNotificationChannel() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (data: SetNotificationChannel) => setNotificationChannelServerFn({ data }),
    onMutate: async ({ type, channel }) => {
      await queryClient.cancelQueries({ queryKey: settingsKey });
      const previous = queryClient.getQueryData<NotificationSettings>(settingsKey);
      queryClient.setQueryData<NotificationSettings>(settingsKey, (settings) =>
        settings ? { ...settings, channels: { ...settings.channels, [type]: channel } } : settings);
      return { previous };
    },
    onError: (error, _variables, context) => {
      queryClient.setQueryData(settingsKey, context?.previous);
      toast.error(toErrorMessage(error, "Couldn't save"));
    },
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: settingsKey });
    },
  });
}
