import { createClientOnlyFn, createIsomorphicFn } from '@tanstack/react-start'
import { rootRouteId, useLoaderData } from '@tanstack/react-router'
import { getCookie } from '@tanstack/react-start/server'

// SSR formats times in the browser's zone, which the browser reports in this cookie.
const TIME_ZONE_COOKIE_NAME = 'tz'
const TIME_ZONE_COOKIE_MAX_AGE = 60 * 60 * 24 * 365

function isTimeZone(value: string | undefined): value is string {
  if (!value) return false
  try {
    new Intl.DateTimeFormat('en-US', { timeZone: value })
    return true
  } catch {
    return false
  }
}

const browserTimeZone = createClientOnlyFn(() => Intl.DateTimeFormat().resolvedOptions().timeZone)

// ponytail: a browser's first request renders UTC; every later request knows its zone.
export const getTimeZone = createIsomorphicFn()
  .server(() => {
    const cookie = getCookie(TIME_ZONE_COOKIE_NAME)
    return isTimeZone(cookie) ? cookie : 'UTC'
  })
  .client(() => browserTimeZone())

export const writeTimeZone = createClientOnlyFn(() => {
  const timeZone = encodeURIComponent(browserTimeZone())
  if (document.cookie.match(/(?:^|; )tz=([^;]+)/)?.[1] === timeZone) return
  const secure = window.location.protocol === 'https:' ? '; Secure' : ''
  document.cookie =
    `${TIME_ZONE_COOKIE_NAME}=${timeZone}; Path=/; Max-Age=${TIME_ZONE_COOKIE_MAX_AGE}; SameSite=Lax${secure}`
})

/** The zone every rendered time uses; the root loader resolves it so SSR and hydration agree. */
export const useTimeZone = () => useLoaderData({ from: rootRouteId, select: (data) => data.timeZone })

/** A wall-clock time ("14:03:05", or "14:03:05.123" with milliseconds) in the given zone. */
export const clock = (timeZone: string, milliseconds = false) => new Intl.DateTimeFormat('en-GB', {
  hourCycle: 'h23', hour: '2-digit', minute: '2-digit', second: '2-digit', fractionalSecondDigits: milliseconds ? 3 : undefined, timeZone,
})
