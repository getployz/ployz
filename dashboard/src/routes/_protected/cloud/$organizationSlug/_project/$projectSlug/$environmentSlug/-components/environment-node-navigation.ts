type EnvironmentParams = { organizationSlug: string; projectSlug: string; environmentSlug: string };

import { linkOptions } from "@tanstack/react-router";
import type { ServicePage } from "../services/$serviceId/-components/service-pages";
import { ENVIRONMENT_SERVICE_ROUTE_TO, ENVIRONMENT_RESOURCE_ROUTE_TO } from "./environment-route-paths";

export type NavigationNode = {
  id: string;
  name: string;
  type: "service" | "volume";
};

export function nodeDestination(
  params: EnvironmentParams,
  node: NavigationNode,
  page?: ServicePage,
) {
  const { organizationSlug, projectSlug, environmentSlug } = params;
  return node.type === "service"
    ? linkOptions({
        to: ENVIRONMENT_SERVICE_ROUTE_TO,
        params: {
          organizationSlug,
          projectSlug,
          environmentSlug,
          serviceId: node.id,
        },
        search: { tab: page },
      })
    : linkOptions({
        to: ENVIRONMENT_RESOURCE_ROUTE_TO,
        params: {
          organizationSlug,
          projectSlug,
          environmentSlug,
          resourceId: node.id,
        },
        search: {},
      });
}
