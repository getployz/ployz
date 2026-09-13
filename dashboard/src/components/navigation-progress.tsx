import { useRouterState } from "@tanstack/react-router";
import { Progress } from "#/components/ui/progress";

export function NavigationProgress() {
  const isLoading = useRouterState({
    select: (state) => state.isLoading,
  });

  if (!isLoading) return null;

  return (
    <Progress
      value={null}
      aria-label="Loading page"
      className="navigation-progress pointer-events-none sticky top-0 h-0 w-full"
    />
  );
}
