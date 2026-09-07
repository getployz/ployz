import { createClientOnlyFn, createIsomorphicFn } from '@tanstack/react-start'
import { getCookie } from '@tanstack/react-start/server'
import { asString } from '#/lib/json'

export type UserTheme = 'light' | 'dark' | 'system'
export type AppTheme = Exclude<UserTheme, 'system'>

const THEME_COOKIE_NAME = 'theme'
const THEME_COOKIE_MAX_AGE = 60 * 60 * 24 * 365

function parseUserTheme<T>(theme: T): UserTheme {
  const text = asString(theme);
  return text === 'light' || text === 'dark' || text === 'system'
    ? text
    : 'system'
}

const readTheme = createClientOnlyFn((): UserTheme => {
  const match = document.cookie.match(/(?:^|; )theme=([^;]+)/)
  const raw = match?.[1]

  return parseUserTheme(raw ? decodeURIComponent(raw) : undefined)
})

export const writeTheme = createClientOnlyFn((userTheme: UserTheme) => {
  const secure = window.location.protocol === 'https:' ? '; Secure' : ''

  document.cookie =
    `${THEME_COOKIE_NAME}=${encodeURIComponent(userTheme)}; Path=/; Max-Age=${THEME_COOKIE_MAX_AGE}; SameSite=Lax${secure}`
})

export const getTheme = createIsomorphicFn()
  .server(() => parseUserTheme(getCookie(THEME_COOKIE_NAME)))
  .client(() => readTheme())

function getSystemTheme(): AppTheme {
  if (import.meta.env.SSR) {
    return 'light'
  }

  return window.matchMedia('(prefers-color-scheme: dark)').matches
    ? 'dark'
    : 'light'
}

export function resolveTheme(userTheme: UserTheme): AppTheme {
  return userTheme === 'system' ? getSystemTheme() : userTheme
}

export const applyTheme = createClientOnlyFn((
  userTheme: UserTheme,
  appTheme = resolveTheme(userTheme)
) => {
  const root = document.documentElement

  root.classList.remove('light', 'dark', 'system')
  root.classList.add(appTheme)

  if (userTheme === 'system') {
    root.classList.add('system')
  }

  root.style.colorScheme = appTheme
})

export function getServerThemeClassName(userTheme: UserTheme) {
  return userTheme === 'system' ? 'system' : userTheme
}

export const themeScript = `(${function () {
  const validThemes = ['light', 'dark', 'system']

  function getStoredTheme() {
    const match = document.cookie.match(/(?:^|; )theme=([^;]+)/)
    const raw = match?.[1]
    const stored = raw ? decodeURIComponent(raw) : 'system'

    return validThemes.includes(stored) ? stored : 'system'
  }

  function getSystemTheme() {
    return window.matchMedia('(prefers-color-scheme: dark)').matches
      ? 'dark'
      : 'light'
  }

  function applyTheme() {
    const userTheme = getStoredTheme()
    const appTheme = userTheme === 'system' ? getSystemTheme() : userTheme
    const root = document.documentElement

    root.classList.remove('light', 'dark', 'system')
    root.classList.add(appTheme)

    if (userTheme === 'system') {
      root.classList.add('system')
    }

    root.style.colorScheme = appTheme
  }

  try {
    applyTheme()
  } catch {
    const root = document.documentElement
    const appTheme = window.matchMedia('(prefers-color-scheme: dark)').matches
      ? 'dark'
      : 'light'

    root.classList.remove('light', 'dark', 'system')
    root.classList.add(appTheme, 'system')
    root.style.colorScheme = appTheme
  }
}.toString()})();`
