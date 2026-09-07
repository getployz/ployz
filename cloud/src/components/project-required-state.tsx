import { FolderKanbanIcon } from "lucide-react";
import { DashboardPage } from "#/components/dashboard-page";
import type { DashboardSection } from "#/components/dashboard-navigation-model";
import { ProjectSwitcher } from "#/components/navigation-switcher";
import {
  Empty,
  EmptyContent,
  EmptyDescription,
  EmptyHeader,
  EmptyMedia,
  EmptyTitle,
} from "#/components/ui/empty";

export function ProjectRequiredState({
  organizationSlug,
  section,
  title,
  description,
}: {
  organizationSlug: string;
  section: DashboardSection;
  title: string;
  description: string;
}) {
  return (
    <DashboardPage className="flex-1">
      <Empty variant="first-run">
        <EmptyHeader>
          <EmptyMedia variant="icon">
            <FolderKanbanIcon />
          </EmptyMedia>
          <EmptyTitle>{title}</EmptyTitle>
          <EmptyDescription>{description}</EmptyDescription>
        </EmptyHeader>
        <EmptyContent>
          <ProjectSwitcher
            organizationSlug={organizationSlug}
            section={section}
            triggerLabel="Select project"
            triggerVariant="outline"
          />
        </EmptyContent>
      </Empty>
    </DashboardPage>
  );
}
