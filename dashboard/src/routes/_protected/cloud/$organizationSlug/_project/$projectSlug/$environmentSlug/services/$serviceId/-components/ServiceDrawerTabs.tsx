import { useNavigate, useSearch } from "@tanstack/react-router";
import { Tabs, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { ServiceSettingsTab } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceSettingsTab";
import { ServiceVariablesTab } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesTab";
import type { ServiceDrawerState } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";
import { servicePageSchema, SERVICE_PAGES } from "./service-pages";
import { Schema } from "effect";

export function ServiceDrawerTabs({
  state,
}: {
  state: ServiceDrawerState;
}) {
  const from = "/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/_canvas/services/$serviceId";
  const { tab } = useSearch({ from });
  const navigate = useNavigate({ from: "/cloud/$organizationSlug/$projectSlug/$environmentSlug/services/$serviceId" });
  return (
    <Tabs
      value={tab ?? "settings"}
      onValueChange={(value) => {
        if (Schema.is(servicePageSchema)(value)) {
          void navigate({ search: (prev) => ({ ...prev, tab: value }), replace: true });
        }
      }}
      className="flex min-h-0 flex-1 flex-col overflow-hidden"
    >
      <TabsList variant="line" className="max-w-full shrink-0 overflow-x-auto max-[860px]:hidden">
        {SERVICE_PAGES.map((page) => <TabsTrigger key={page.id} value={page.id}>{page.label}</TabsTrigger>)}
      </TabsList>

      <ServiceSettingsTab state={state} />
      <ServiceVariablesTab state={state} />
    </Tabs>
  );
}
