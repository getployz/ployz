import { createServerFn } from "@tanstack/react-start";
import { SetNotificationChannel } from "#/modules/notifications/notification-types";
import { getNotificationSettings, setNotificationChannel } from "#/modules/notifications/preferences.server";
import { authenticated, runActor, strictValidator } from "#/server/tanstack";


export const getNotificationSettingsServerFn = createServerFn({ method: "GET" })
  .middleware(authenticated)
  .handler(({ context }) => runActor(context, getNotificationSettings(context.actor)));

export const setNotificationChannelServerFn = createServerFn({ method: "POST" })
  .middleware(authenticated)
  .validator(strictValidator(SetNotificationChannel))
  .handler(({ context, data }) => runActor(context, setNotificationChannel(context.actor.userId, data.type, data.channel)));
