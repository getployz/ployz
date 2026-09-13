import {
  Empty,
  EmptyDescription,
  EmptyHeader,
  EmptyTitle,
} from "#/components/ui/empty";
import { DashboardPage } from "#/components/dashboard-page";

export function EnvironmentPlaceholder({
  title,
  description = "Nothing to show yet.",
}: {
  title: string;
  description?: string;
}) {
  return (
    <DashboardPage density="compact" width="content">
      <Empty variant="placeholder">
        <EmptyHeader>
          <EmptyTitle>{title}</EmptyTitle>
          <EmptyDescription>{description}</EmptyDescription>
        </EmptyHeader>
      </Empty>
    </DashboardPage>
  );
}
