import { createServerFn } from "@tanstack/react-start";
import { SetNotificationChannel } from "#/modules/notifications/notification-types";
import { getNotificationSettings, setNotificationChannel } from "#/modules/notifications/preferences.server";
import { actorMiddleware, publicErrorMiddleware, runActor, strictValidator } from "#/server/tanstack";

const middleware = [publicErrorMiddleware, actorMiddleware] as const;

export const getNotificationSettingsServerFn = createServerFn({ method: "GET" })
  .middleware(middleware)
  .handler(({ context }) => runActor(context, getNotificationSettings(context.actor)));

export const setNotificationChannelServerFn = createServerFn({ method: "POST" })
  .middleware(middleware)
  .validator(strictValidator(SetNotificationChannel))
  .handler(({ context, data }) => runActor(context, setNotificationChannel(context.actor.userId, data.type, data.channel)));
