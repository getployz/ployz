import { Tabs, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { ServiceDeploymentsTab } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceDeploymentsTab";
import { ServiceSettingsTab } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingsTab";
import { ServiceVariablesTab } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesTab";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";

export function ServiceDrawerTabs({
  state,
}: {
  state: ServiceDrawerState;
}) {
  return (
    <Tabs
      defaultValue="settings"
      className="flex min-h-0 flex-1 flex-col overflow-hidden px-6 pb-6"
    >
      <TabsList variant="line">
        <TabsTrigger value="settings">Settings</TabsTrigger>
        <TabsTrigger value="variables">Variables</TabsTrigger>
        <TabsTrigger value="deployments">Deployments</TabsTrigger>
      </TabsList>

      <ServiceSettingsTab state={state} />
      <ServiceVariablesTab state={state} />
      <ServiceDeploymentsTab />
    </Tabs>
  );
}
