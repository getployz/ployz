import { cn } from "#/lib/utils.ts"

function Skeleton({
  className,
  variant = "default",
  ...props
}: React.ComponentProps<"div"> & {
  /** "inline" sits inside running text and takes the text's tone. */
  variant?: "default" | "inline"
}) {
  return (
    <div
      data-slot="skeleton"
      data-variant={variant}
      className={cn(
        "animate-pulse rounded-md bg-muted",
        variant === "inline" && "inline-block h-3 w-12 rounded-sm bg-muted-foreground/40 align-middle",
        className
      )}
      {...props}
    />
  )
}

export { Skeleton }
