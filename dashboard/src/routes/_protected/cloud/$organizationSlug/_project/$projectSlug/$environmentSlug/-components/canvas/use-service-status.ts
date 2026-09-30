import { use } from "react";
import { useLoaderData, useParams } from "@tanstack/react-router";
import { useInFlightTargets } from "#/modules/config-store/store-view.queries";
import { useRuntimeService } from "#/providers/runtime-provider";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { awaitsDeploy, deployChip, nodeIssues, runtimeLine } from "./node-status";
import { RuntimeLensContext } from "./RuntimeLensProvider";
import { useVolumeFill } from "./use-volume-fill";
import type { StoreCanvasService } from "./types";

/** A Service's chip, status line and issues, which its card and its panel both show. */
export function useServiceStatus({ service, domains, changeCount, runtimeIdentity, desiredReplicas, trays }: StoreCanvasService) {
  const { organizationSlug } = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const { store } = useLoaderData({ from: ENVIRONMENT_ROUTE_FROM });
  const { runtime } = useRuntimeService(runtimeIdentity ?? "");
  const lens = use(RuntimeLensContext);
  const fillOf = useVolumeFill();
  const inFlight = useInFlightTargets(organizationSlug, store);
  const chip = deployChip(service, changeCount, inFlight);
  const status = runtimeLine(service, runtime, { lens, desiredReplicas, chip, awaited: awaitsDeploy(service, inFlight) });
  const issues = nodeIssues(status, domains, trays.map(({ volume }) => ({ volume, fill: fillOf(volume.id) })));
  return { chip, status, issues };
}
