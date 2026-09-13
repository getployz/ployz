import { cva, type VariantProps } from "class-variance-authority";
import { cn } from "#/lib/utils";

const dashboardPageVariants = cva("mx-auto flex w-full flex-col", {
  variants: {
    density: {
      compact: "gap-4 px-4 py-4 md:px-6 md:py-5",
      standard: "gap-6 px-4 py-6 md:px-6 md:py-8",
      spacious: "gap-8 px-4 py-8 md:px-6 md:py-12",
    },
    width: {
      content: "max-w-5xl",
      standard: "max-w-6xl",
      wide: "max-w-7xl",
    },
  },
  defaultVariants: {
    density: "standard",
    width: "standard",
  },
});

export function DashboardPage({
  className,
  density,
  width,
  ...props
}: React.ComponentProps<"main"> & VariantProps<typeof dashboardPageVariants>) {
  return (
    <main
      className={cn(dashboardPageVariants({ density, width }), className)}
      {...props}
    />
  );
}
