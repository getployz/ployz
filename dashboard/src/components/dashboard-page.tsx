import type { ReactNode } from "react";
import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "#/lib/utils";

const dashboardPageVariants = cva("mx-auto flex w-full flex-col gap-6 px-4 py-6 md:px-6 md:py-8", {
  variants: {
    width: {
      content: "max-w-5xl",
      standard: "max-w-6xl",
    },
  },
  defaultVariants: {
    width: "standard",
  },
});

/** A page's column. `title` is the page's one h1, in a row with its `actions`, lined up with the content below. */
export function DashboardPage({
  className,
  width,
  title,
  actions,
  children,
  ...props
}: React.ComponentProps<"div"> & VariantProps<typeof dashboardPageVariants> & { title?: ReactNode; actions?: ReactNode }) {
  return (
    <div
      className={cn(dashboardPageVariants({ width }), className)}
      {...props}
    >
      {title === undefined ? null : (
        <div className="flex min-h-9 flex-wrap items-center gap-3">
          <h1 className="min-w-0 truncate text-2xl font-semibold">{title}</h1>
          {actions === undefined ? null : <div className="ml-auto flex shrink-0 items-center gap-2">{actions}</div>}
        </div>
      )}
      {children}
    </div>
  );
}
