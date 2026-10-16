import { type QueryKey, queryOptions, useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { toast } from "sonner";
import { toErrorMessage } from "#/lib/error-message";
import type { RangeSpec, RequestWindow } from "./analytics-model";
import {
  createGoalServerFn,
  deleteGoalServerFn,
  readAnalyticsViewServerFn,
  readApiViewServerFn,
  readErrorsViewServerFn,
  turnOnAnalyticsServerFn,
} from "./analytics.functions";

export type AnalyticsScope = { readonly organizationSlug: string; readonly environmentId: string };
export type AnalyticsView = Awaited<ReturnType<typeof readAnalyticsViewServerFn>>;
export type AnalyticsGoal = Extract<AnalyticsView, { gate: "on" }>["goals"][number];
export type ApiView = Awaited<ReturnType<typeof readApiViewServerFn>>;
export type ErrorsView = Awaited<ReturnType<typeof readErrorsViewServerFn>>;

/** One window of the API or Errors view, for every service or one. */
export type RequestRead = { readonly window: RequestWindow; readonly service: string | undefined };

const requestInput = (scope: AnalyticsScope, read: RequestRead) =>
  ({ ...scope, window: read.window, ...(read.service !== undefined && { service: read.service }) });

export function apiViewQueryOptions(scope: AnalyticsScope, read: RequestRead) {
  return queryOptions({
    queryKey: ["analytics-requests", scope.environmentId, "api", read.window, read.service ?? null] as const,
    queryFn: () => readApiViewServerFn({ data: requestInput(scope, read) }),
    staleTime: 30_000,
  });
}

export function errorsViewQueryOptions(scope: AnalyticsScope, read: RequestRead) {
  return queryOptions({
    queryKey: ["analytics-requests", scope.environmentId, "errors", read.window, read.service ?? null] as const,
    queryFn: () => readErrorsViewServerFn({ data: requestInput(scope, read) }),
    staleTime: 30_000,
  });
}

/** The Environment's analytics for one range and page filter, or which gate is closed. */
export function analyticsViewQueryOptions(scope: AnalyticsScope, range: RangeSpec, page: string | undefined) {
  return queryOptions({
    queryKey: ["analytics", scope.environmentId, range, page ?? null] as const,
    queryFn: () => readAnalyticsViewServerFn({ data: { ...scope, range, ...(page !== undefined && { page }) } }),
    staleTime: 60_000,
  });
}

export function useAnalyticsView(scope: AnalyticsScope, range: RangeSpec, page: string | undefined) {
  return useSuspenseQuery(analyticsViewQueryOptions(scope, range, page)).data;
}

const viewKey = (scope: AnalyticsScope) => ["analytics", scope.environmentId] as const;

/** Turn on: counts from now, in the browser's time zone unless the Environment already counted in one. */
export function useTurnOnAnalytics(scope: AnalyticsScope) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => turnOnAnalyticsServerFn({ data: { ...scope, timezone: Intl.DateTimeFormat().resolvedOptions().timeZone } }),
    onError: (error) => { toast.error(toErrorMessage(error, "Could not turn on analytics")); },
    onSettled: () => Promise.all([
      queryClient.invalidateQueries({ queryKey: viewKey(scope) }),
      queryClient.invalidateQueries({ queryKey: ["analytics-requests", scope.environmentId] }),
    ]),
  });
}

type GoalsUpdate = (goals: AnalyticsGoal[]) => AnalyticsGoal[];

/** Rewrites the goals of every cached range, returning what to put back if Cloud refuses. */
function useOptimisticGoals(scope: AnalyticsScope) {
  const queryClient = useQueryClient();
  const apply = async (update: GoalsUpdate) => {
    await queryClient.cancelQueries({ queryKey: viewKey(scope) });
    const previous = queryClient.getQueriesData<AnalyticsView>({ queryKey: viewKey(scope) });
    queryClient.setQueriesData<AnalyticsView>({ queryKey: viewKey(scope) }, (view) =>
      view?.gate === "on" ? { ...view, goals: update(view.goals) } : view);
    return { previous };
  };
  const restore = (previous: ReadonlyArray<readonly [QueryKey, AnalyticsView | undefined]> | undefined) => {
    for (const [key, view] of previous ?? []) queryClient.setQueryData(key, view);
  };
  const settle = () => queryClient.invalidateQueries({ queryKey: viewKey(scope) });
  return { apply, restore, settle };
}

export function useCreateGoal(scope: AnalyticsScope) {
  const goals = useOptimisticGoals(scope);
  return useMutation({
    mutationFn: (goal: { name: string; pathPattern: string }) => createGoalServerFn({ data: { ...scope, ...goal } }),
    onMutate: (goal) => goals.apply((current) => [
      ...current,
      { id: `pending:${crypto.randomUUID()}`, name: goal.name, pathPattern: goal.pathPattern, visitors: 0, conversions: 0 },
    ]),
    onError: (error, _goal, context) => {
      goals.restore(context?.previous);
      toast.error(toErrorMessage(error, "Could not add the goal"));
    },
    onSettled: goals.settle,
  });
}

export function useDeleteGoal(scope: AnalyticsScope) {
  const goals = useOptimisticGoals(scope);
  return useMutation({
    mutationFn: (id: string) => deleteGoalServerFn({ data: { ...scope, id } }),
    onMutate: (id) => goals.apply((current) => current.filter((goal) => goal.id !== id)),
    onError: (error, _id, context) => {
      goals.restore(context?.previous);
      toast.error(toErrorMessage(error, "Could not delete the goal"));
    },
    onSettled: goals.settle,
  });
}
