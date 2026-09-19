import type { ReactNode } from "react";
import { Separator } from "#/components/ui/separator";
import { TabsContent } from "#/components/ui/tabs";
import { ServiceBuildSection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceBuildSection";
import { ServiceCommandsSection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceCommandsSection";
import { ServiceDangerSection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceDangerSection";
import { ServiceHealthcheckSection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceHealthcheckSection";
import { ServiceNetworkingSection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceNetworkingSection";
import { ServiceRestartPolicySection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceRestartPolicySection";
import { ServiceScaleSection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceScaleSection";
import { ServiceSettingsSection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingsSection";
import { ServiceSourceSection } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSourceSection";
import {
  SERVICE_SETTINGS_SECTIONS,
  type ServiceSettingsSectionId,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/service-settings-sections";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceSettingsTab({ state }: { state: ServiceDrawerState }) {
  const bodies = {
    source: <ServiceSourceSection state={state} />,
    networking: <ServiceNetworkingSection state={state} />,
    scale: <ServiceScaleSection state={state} />,
    build: <ServiceBuildSection state={state} />,
    deploy: (
      <div className="flex flex-col gap-6">
        <ServiceCommandsSection state={state} />
        <Separator />
        <ServiceHealthcheckSection state={state} />
        <Separator />
        <ServiceRestartPolicySection state={state} />
      </div>
    ),
    danger: <ServiceDangerSection state={state} />,
  } satisfies Record<ServiceSettingsSectionId, ReactNode>;

  return (
    <TabsContent value="settings" className="mt-3 min-h-0 flex-1 overflow-hidden">
      <div className="h-full overflow-y-auto pr-1 pb-8">
        <div className="mx-auto flex w-full max-w-2xl flex-col gap-6 **:data-[slot=field-group]:gap-4">
          {SERVICE_SETTINGS_SECTIONS.map((section) => (
            <ServiceSettingsSection
              key={section.id}
              id={section.id}
              title={section.label}
              description={section.description}
              variant={section.id === "danger" ? "danger" : "default"}
            >
              {bodies[section.id]}
            </ServiceSettingsSection>
          ))}
        </div>
      </div>
    </TabsContent>
  );
}

