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

let themeSnapshot: `${UserTheme}:${AppTheme}` | null = null

function toThemeSnapshot(userTheme: UserTheme): `${UserTheme}:${AppTheme}` {
  return `${userTheme}:${resolveTheme(userTheme)}`
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
    // SAFETY: theme snapshots are always `${UserTheme}:${AppTheme}`; String.split returns string[].
    const [userTheme] = getThemeSnapshot().split(':') as [UserTheme, AppTheme]

    if (userTheme === 'system') {
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

  const snapshot = useSyncExternalStore(
    subscribeToTheme,
    getThemeSnapshot,
    () => toThemeSnapshot(theme),
  )
  // SAFETY: theme snapshots are always `${UserTheme}:${AppTheme}`; String.split returns string[].
  const [userTheme, appTheme] = snapshot.split(':') as [UserTheme, AppTheme]
  const value = useMemo(
    () => ({ userTheme, appTheme, setTheme }),
    [userTheme, appTheme],
  )

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
