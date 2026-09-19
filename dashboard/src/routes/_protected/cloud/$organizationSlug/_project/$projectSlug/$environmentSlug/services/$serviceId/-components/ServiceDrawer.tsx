import type {
  ServiceDrawerState,
  ServiceRouteParams,
} from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/useServiceDrawerState";
import { ServiceDrawerHeader } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceDrawerHeader";
import { ServiceDrawerTabs } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceDrawerTabs";

export function ServiceDrawer({
  params,
  state,
}: {
  params: ServiceRouteParams;
  state: ServiceDrawerState;
}) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <ServiceDrawerHeader params={params} state={state} />
      <div className="flex min-h-0 flex-1 flex-col px-4 pb-4">
        <ServiceDrawerTabs state={state} />
      </div>
    </div>
  );
}
