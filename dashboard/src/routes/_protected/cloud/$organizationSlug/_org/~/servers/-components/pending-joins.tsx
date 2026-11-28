import type { JoinApproval, Signed } from "@ployz/sdk";
import { passkeyJoinStep } from "@ployz/sdk/authority";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Button } from "#/components/ui/button";
import { type OnlyPasskey, PasskeyDeclined, signWithPasskey } from "#/modules/passkeys/passkey-statement";
import { useRoleGate } from "#/modules/organization/members.queries";
import { approvePasskeyJoinServerFn, pendingPasskeyJoinsServerFn } from "#/modules/machines/add-server/passkey-join.functions";
import { pendingJoinsKey, pendingJoinsQueryOptions } from "#/modules/machines/add-server/passkey-join.queries";

/** How long a passkey's approval stays open, at most: the join's own window may end sooner. */
const APPROVAL_WINDOW_MS = 10 * 60_000;

type Pending = Awaited<ReturnType<typeof pendingPasskeyJoinsServerFn>>[number];

/**
 * `join`, the Server's own draft, as the one step of a bundle the person's passkey answers. The bundle and step are
 * written here by the ployz-core code the Servers check them with, so the passkey signs what this page shows.
 */
export async function answerJoin(join: JoinApproval, only?: OnlyPasskey, now = Date.now()): Promise<Signed> {
  const draft = passkeyJoinStep(join, crypto.randomUUID().replaceAll("-", ""), now, Math.min(join.expires_at, now + APPROVAL_WINDOW_MS));
  try {
    return { body: draft.step, signatures: [], proposal: await signWithPasskey(draft.bundle, "workflow", only && [only]) };
  } catch (error) {
    if (error instanceof PasskeyDeclined) throw error;
    if (error instanceof Error && error.name === "NotAllowedError") throw new PasskeyDeclined("Your passkey didn't answer. Try again.");
    throw new PasskeyDeclined("Your browser couldn't use a passkey here.");
  }
}

/** Servers waiting for a person to approve their join, each approved with that person's passkey. */
export function PendingJoins({ organizationSlug }: { organizationSlug: string }) {
  const gate = useRoleGate(organizationSlug, "servers:add");
  const pending = useQuery({ ...pendingJoinsQueryOptions(organizationSlug), enabled: gate.allowed });
  const joins = pending.data ?? [];
  if (joins.length === 0) return null;
  return (
    <section aria-labelledby="pending-joins-heading" className="flex flex-col gap-2.5">
      <h2 id="pending-joins-heading" className="text-sm font-semibold">Waiting for approval</h2>
      <p className="text-sm text-muted-foreground">
        Approve a server only if you added it.
      </p>
      <ul className="flex flex-col divide-y rounded-lg border">
        {joins.map((pending) => <PendingJoin key={pending.join.machine.id} organizationSlug={organizationSlug} pending={pending} />)}
      </ul>
    </section>
  );
}

/**
 * Why this person can't approve `pending` here, or null when they can. A Server whose install command named a key takes
 * only that key's approval, and a join answered under any other is spent on a refusal.
 */
export function cannotApprove({ expected, credentialId, code }: Pending): string | null {
  if (expected === null || credentialId !== null) return null;
  return expected.kind === "passkey"
    ? `This server trusts only passkey ${expected.code}, which isn't yours. Ask the person who added it to approve it.`
    : `This server trusts only device ${expected.code}. On that computer, run ployz device approve --join ${code}.`;
}

function PendingJoin({ organizationSlug, pending }: { organizationSlug: string; pending: Pending }) {
  const { join, expected, credentialId, code } = pending;
  const queryClient = useQueryClient();
  const only = credentialId === null || expected === null ? undefined : { credentialId, code: expected.code };
  const approve = useMutation({
    mutationFn: async () => approvePasskeyJoinServerFn({ data: { organizationSlug, approval: JSON.stringify(await answerJoin(join, only)) } }),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: pendingJoinsKey(organizationSlug) }),
  });
  const refused = cannotApprove(pending);
  return (
    <li className="flex flex-wrap items-center gap-3 px-3 py-2.5 text-sm">
      <span className="font-medium">{join.machine.name}</span>
      <span className="font-mono text-xs text-muted-foreground">Join code {code}</span>
      <span className="ml-auto flex items-center gap-3">
        {refused === null ? null : <span className="text-muted-foreground">{refused}</span>}
        {approve.isSuccess ? <span className="text-muted-foreground">Approved. Enroll it again.</span> : null}
        {approve.isError ? <span role="alert" className="text-destructive">{approve.error.message}</span> : null}
        {approve.isSuccess || refused !== null ? null : (
          <Button variant="outline" disabled={approve.isPending} onClick={() => approve.mutate()}>
            {approve.isPending ? "Waiting for your passkey…" : "Approve with passkey"}
          </Button>
        )}
      </span>
    </li>
  );
}
