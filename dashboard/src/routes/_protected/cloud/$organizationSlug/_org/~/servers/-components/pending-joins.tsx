import type { JoinApproval, Signed } from "@ployz/sdk";
import { passkeyJoinStep } from "@ployz/sdk/authority";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "#/components/ui/button";
import { PasskeyRefused, signWithPasskey } from "#/modules/approval-session/passkey-statement";
import { useRoleGate } from "#/modules/organization/members.queries";
import { approvePasskeyJoinServerFn, pendingPasskeyJoinsServerFn } from "#/modules/machines/waiting/passkey-join.functions";
import { WAITING_POLL_MS } from "#/modules/machines/waiting/waiting.queries";

/** How long a passkey's approval stays open, at most: the join's own window may end sooner. */
const APPROVAL_WINDOW_MS = 10 * 60_000;

const pendingJoinsKey = (organizationSlug: string) => ["pending-joins", organizationSlug] as const;

/**
 * `join`, the Server's own draft, as the one step of a bundle the person's passkey answers. The bundle and step are
 * written here by the ployz-core code the Servers check them with, so the passkey signs what this page shows.
 */
export async function answerJoin(join: JoinApproval, now = Date.now()): Promise<Signed> {
  const draft = passkeyJoinStep(join, crypto.randomUUID().replaceAll("-", ""), now, Math.min(join.expires_at, now + APPROVAL_WINDOW_MS));
  try {
    return { body: draft.step, signatures: [], proposal: await signWithPasskey(draft.bundle, "workflow") };
  } catch (error) {
    if (error instanceof PasskeyRefused) throw error;
    if (error instanceof Error && error.name === "NotAllowedError") throw new PasskeyRefused("Your passkey didn't answer. Try again.");
    throw new PasskeyRefused("Your browser couldn't use a passkey here.");
  }
}

/** Servers waiting for a person to approve their join, each approved with that person's passkey. */
export function PendingJoins({ organizationSlug }: { organizationSlug: string }) {
  const gate = useRoleGate(organizationSlug, "servers:add");
  const pending = useQuery({
    queryKey: pendingJoinsKey(organizationSlug),
    queryFn: () => pendingPasskeyJoinsServerFn({ data: { organizationSlug } }),
    refetchInterval: WAITING_POLL_MS,
    enabled: gate.allowed,
  });
  const joins = pending.data ?? [];
  if (joins.length === 0) return null;
  return (
    <section aria-labelledby="pending-joins-heading" className="flex flex-col gap-2.5">
      <h2 id="pending-joins-heading" className="text-sm font-semibold">Waiting for approval</h2>
      <p className="text-sm text-muted-foreground">
        Approve a server only if you added it.
      </p>
      <ul className="flex flex-col divide-y rounded-lg border">
        {joins.map((join) => <PendingJoin key={join.machine.id} organizationSlug={organizationSlug} join={join} />)}
      </ul>
    </section>
  );
}

function PendingJoin({ organizationSlug, join }: { organizationSlug: string; join: JoinApproval }) {
  const queryClient = useQueryClient();
  const approve = useMutation({
    mutationFn: async () => approvePasskeyJoinServerFn({ data: { organizationSlug, approval: JSON.stringify(await answerJoin(join)) } }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: pendingJoinsKey(organizationSlug) }),
  });
  return (
    <li className="flex flex-wrap items-center gap-3 px-3 py-2.5 text-sm">
      <span className="font-medium">{join.machine.name}</span>
      <span className="font-mono text-xs text-muted-foreground">{join.machine.id}</span>
      <span className="ml-auto flex items-center gap-3">
        {approve.isSuccess ? <span className="text-muted-foreground">Approved. Enroll it again.</span> : null}
        {approve.isError ? <span role="alert" className="text-destructive">{approve.error.message}</span> : null}
        {approve.isSuccess ? null : (
          <Button variant="outline" disabled={approve.isPending} onClick={() => approve.mutate()}>
            {approve.isPending ? "Waiting for your passkey…" : "Approve with passkey"}
          </Button>
        )}
      </span>
    </li>
  );
}
