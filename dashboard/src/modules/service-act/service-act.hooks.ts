import { useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import type { ServiceVerb } from "@ployz/sdk";
import { usePendingAttempt } from "#/hooks/use-pending-attempt";
import { toErrorMessage } from "#/lib/error-message";
import { actAlert, actBadge } from "./service-act-view";
import { actServiceServerFn } from "./service-act.functions";
import { serviceActKeys, useLatestServiceAct } from "./service-act.queries";

type Subject = {
  readonly organizationSlug: string;
  readonly environmentId: string;
  readonly service: string;
  /** The Environment is Protected and the person can't protect it: an owner or admin approves the stop or restart. */
  readonly asks: boolean;
};

/**
 * The request id is minted here so a retried POST admits the same run instead of a second one.
 */
export function useServiceAct(subject: Subject) {
  const queryClient = useQueryClient();
  const { organizationSlug, environmentId, service } = subject;
  const latest = useLatestServiceAct({ organizationSlug, environmentId, service });
  const [requested, setRequested, arrived] = usePendingAttempt<{ readonly verb: ServiceVerb; readonly id: string }, typeof latest>(
    latest,
    ({ id }, row) => row?.id === id,
  );

  const act = (verb: ServiceVerb) => {
    const requestId = crypto.randomUUID();
    setRequested({ verb, id: requestId });
    actServiceServerFn({ data: { organizationSlug, environmentId, service, verb, requestId } }).then(
      (result) => {
        if ("requested" in result) {
          setRequested((current) => (current?.id === requestId ? null : current));
          toast("Waiting on an owner or admin.", { action: { label: "Open", onClick: () => window.open(result.url, "_blank", "noopener") } });
          return;
        }
        setRequested((current) => (current?.id === requestId ? { verb, id: result.id } : current));
        void queryClient.invalidateQueries({ queryKey: serviceActKeys.all });
      },
      (error: Error) => {
        setRequested((current) => (current?.id === requestId ? null : current));
        toast.error(toErrorMessage(error, `Could not ${verb} ${subject.service}`));
      },
    );
  };

  const pending = requested !== null && !arrived ? requested.verb : null;
  return { badge: actBadge(latest, pending), alert: pending === null ? actAlert(latest) : null, act };
}

export type ServiceAct = ReturnType<typeof useServiceAct>;
