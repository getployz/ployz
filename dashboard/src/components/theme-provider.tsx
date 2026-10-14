import {
  createContext,
  use,
  useEffect,
  useMemo,
  useSyncExternalStore,
  type PropsWithChildren,
} from 'react'
import { ScriptOnce } from '@tanstack/react-router'
import {
  applyTheme,
  resolveTheme,
  themeScript,
  writeTheme,
  type AppTheme,
  type UserTheme,
} from '#/utils/theme'

type ThemeContextValue = {
  userTheme: UserTheme
  appTheme: AppTheme
  setTheme: (theme: UserTheme) => void
}

const ThemeContext = createContext<ThemeContextValue | null>(null)
const systemThemeQuery = '(prefers-color-scheme: dark)'
const themeListeners = new Set<() => void>()

type ThemeSnapshot = {
  readonly userTheme: UserTheme
  readonly appTheme: AppTheme
}

let themeSnapshot: ThemeSnapshot | null = null

/** Reuses the current snapshot when nothing changed, so React sees the same object and skips the render. */
function toThemeSnapshot(userTheme: UserTheme): ThemeSnapshot {
  const appTheme = resolveTheme(userTheme)
  return themeSnapshot?.userTheme === userTheme &&
    themeSnapshot.appTheme === appTheme
    ? themeSnapshot
    : { userTheme, appTheme }
}

function publishThemeSnapshot(userTheme: UserTheme) {
  const nextSnapshot = toThemeSnapshot(userTheme)

  applyTheme(userTheme)

  if (themeSnapshot === nextSnapshot) {
    return
  }

  themeSnapshot = nextSnapshot
  for (const listener of themeListeners) {
    listener()
  }
}

function getThemeSnapshot() {
  themeSnapshot ??= toThemeSnapshot('system')

  return themeSnapshot
}

function subscribeToTheme(listener: () => void) {
  themeListeners.add(listener)

  const mediaQuery = window.matchMedia(systemThemeQuery)
  const handleSystemThemeChange = () => {
    if (getThemeSnapshot().userTheme === 'system') {
      publishThemeSnapshot('system')
    }
  }

  mediaQuery.addEventListener('change', handleSystemThemeChange)

  return () => {
    themeListeners.delete(listener)
    mediaQuery.removeEventListener('change', handleSystemThemeChange)
  }
}

function setTheme(nextUserTheme: UserTheme) {
  writeTheme(nextUserTheme)
  publishThemeSnapshot(nextUserTheme)
}

export function ThemeProvider({
  children,
  theme,
}: PropsWithChildren<{ theme: UserTheme }>) {
  themeSnapshot ??= toThemeSnapshot(theme)

  useEffect(() => {
    publishThemeSnapshot(theme)
  }, [theme])

  const serverSnapshot = useMemo(() => toThemeSnapshot(theme), [theme])
  const snapshot = useSyncExternalStore(
    subscribeToTheme,
    getThemeSnapshot,
    () => serverSnapshot,
  )
  const value = useMemo(() => ({ ...snapshot, setTheme }), [snapshot])

  return (
    <ThemeContext value={value}>
      <ScriptOnce>{themeScript}</ScriptOnce>
      {children}
    </ThemeContext>
  )
}

export function useTheme() {
  const value = use(ThemeContext)

  if (!value) {
    throw new Error('useTheme must be used within ThemeProvider')
  }

  return value
}
