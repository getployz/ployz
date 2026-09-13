import { ToggleGroup, ToggleGroupItem } from '#/components/ui/toggle-group'
import { useTheme } from '#/components/theme-provider'

export default function ThemeToggle() {
  const { userTheme, setTheme } = useTheme()

  return (
    <ToggleGroup
      value={[userTheme]}
      onValueChange={(value) => {
        const nextTheme = value[0]

        if (
          nextTheme === 'light' ||
          nextTheme === 'dark' ||
          nextTheme === 'system'
        ) {
          setTheme(nextTheme)
        }
      }}
      variant="outline"
      size="sm"
      aria-label="Theme mode"
    >
      <ToggleGroupItem value="light" aria-label="Use light theme">
        Light
      </ToggleGroupItem>
      <ToggleGroupItem value="dark" aria-label="Use dark theme">
        Dark
      </ToggleGroupItem>
      <ToggleGroupItem value="system" aria-label="Use system theme">
        Auto
      </ToggleGroupItem>
    </ToggleGroup>
  )
}
