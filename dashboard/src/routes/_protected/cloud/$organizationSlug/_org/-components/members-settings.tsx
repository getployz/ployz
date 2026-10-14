import { useState } from "react";
import { useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { getRouteApi, useNavigate } from "@tanstack/react-router";
import { UserPlusIcon } from "lucide-react";
import { toast } from "sonner";
import { CopyButton } from "#/components/copy-button";
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from "#/components/ui/alert-dialog";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { Spinner } from "#/components/ui/spinner";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "#/components/ui/table";
import { toErrorMessage } from "#/lib/error-message";
import {
  inviteMemberServerFn,
  leaveOrganizationServerFn,
  removeMemberServerFn,
  revokeInviteServerFn,
  setMemberRoleServerFn,
} from "#/modules/organization/members.functions";
import { membersKey, organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import { SettingsSection } from "#/routes/_protected/cloud/$organizationSlug/-components/SettingsSection";

type Role = "owner" | "admin" | "member";
type InviteRole = Exclude<Role, "owner">;

const ROLE_LABELS = { owner: "Owner", admin: "Admin", member: "Member" } satisfies Record<Role, string>;

const organizationRoute = getRouteApi("/_protected/cloud/$organizationSlug");

/** Both tables share these widths so their columns line up. */
const WIDTHS = { role: "w-28 sm:w-32", date: "hidden w-32 sm:table-cell", actions: "w-20 sm:w-24" };

const formatDate = (iso: string) => new Date(iso).toLocaleDateString(undefined, { dateStyle: "medium" });

type Pending = { kind: "remove"; email: string; name: string } | { kind: "leave" } | null;

export function MembersSettings({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { data } = useSuspenseQuery(organizationMembersQueryOptions(organizationSlug));
  const { organizationName } = organizationRoute.useLoaderData();
  const [inviting, setInviting] = useState(false);
  const [confirming, setConfirming] = useState<Pending>(null);
  const queryKey = membersKey(organizationSlug);
  const refresh = () => queryClient.invalidateQueries({ queryKey });
  const failed = (fallback: string) => (cause: unknown) => toast.error(toErrorMessage(cause, fallback));
  const optimistic = <Input,>(fallback: string, apply: (current: typeof data, input: Input) => typeof data) => ({
    onMutate: async (input: Input) => {
      await queryClient.cancelQueries({ queryKey });
      const previous = queryClient.getQueryData<typeof data>(queryKey);
      if (previous) queryClient.setQueryData(queryKey, apply(previous, input));
      return { previous };
    },
    onError: (cause: unknown, _input: Input, context: { previous: typeof data | undefined } | undefined) => {
      if (context?.previous) queryClient.setQueryData(queryKey, context.previous);
      failed(fallback)(cause);
    },
    onSettled: refresh,
  });

  const setRole = useMutation({
    mutationFn: (input: { member: string; role: Role }) => setMemberRoleServerFn({ data: { organizationSlug, ...input } }),
    ...optimistic<{ member: string; role: Role }>("Couldn't change the role.", (current, input) => ({
      ...current,
      members: current.members.map((row) => row.email === input.member ? { ...row, role: input.role } : row),
    })),
  });
  const remove = useMutation({
    mutationFn: (member: string) => removeMemberServerFn({ data: { organizationSlug, member } }),
    ...optimistic<string>("Couldn't remove the member.", (current, member) => ({
      ...current,
      members: current.members.filter((row) => row.email !== member),
    })),
  });
  const leave = useMutation({
    mutationFn: () => leaveOrganizationServerFn({ data: { organizationSlug } }),
    // The session acts in another Organization now, or none.
    onSuccess: () => navigate({ to: "/cloud", replace: true, reloadDocument: true }),
  });
  const revoke = useMutation({
    mutationFn: (invite: string) => revokeInviteServerFn({ data: { organizationSlug, invite } }),
    ...optimistic<string>("Couldn't revoke the invite.", (current, invite) => ({
      ...current,
      invites: current.invites.filter((row) => row.id !== invite),
    })),
  });

  const yourRole = data.members.find((row) => row.you)?.role ?? null;
  const manager = yourRole === "owner" || yourRole === "admin";
  const roles: readonly Role[] = yourRole === "owner" ? ["owner", "admin", "member"] : ["admin", "member"];

  return (
    <div className="flex flex-col gap-6">
      <SettingsSection
        id="members"
        title="Members"
        action={manager ? (
          <Button variant="outline" onClick={() => setInviting(true)}>
            <UserPlusIcon data-icon="inline-start" />
            Invite
          </Button>
        ) : null}
      >
        <Table className="table-fixed">
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead className={WIDTHS.role}>Role</TableHead>
              <TableHead className={WIDTHS.date}>Joined</TableHead>
              <TableHead className={WIDTHS.actions}><span className="sr-only">Actions</span></TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {data.members.map((row) => {
              const editable = manager && !row.you && (yourRole === "owner" || row.role !== "owner");
              return (
                <TableRow key={row.email}>
                  <TableCell className="whitespace-normal wrap-anywhere">
                    <div className="flex flex-col">
                      <span>{row.name}{row.you ? <span className="text-muted-foreground"> (you)</span> : null}</span>
                      <span className="text-xs text-muted-foreground">{row.email}</span>
                    </div>
                  </TableCell>
                  <TableCell>
                    {editable ? (
                      <NativeSelect
                        size="sm"
                        className="w-24 sm:w-28"
                        aria-label={`Role of ${row.name}`}
                        value={row.role}
                        onChange={(event) => {
                          const role = roles.find((candidate) => candidate === event.target.value);
                          if (role) setRole.mutate({ member: row.email, role });
                        }}
                      >
                        {roles.map((role) => <NativeSelectOption key={role} value={role}>{ROLE_LABELS[role]}</NativeSelectOption>)}
                      </NativeSelect>
                    ) : ROLE_LABELS[row.role]}
                  </TableCell>
                  <TableCell className="hidden text-muted-foreground sm:table-cell">{formatDate(row.joined_at)}</TableCell>
                  <TableCell className="text-right">
                    {row.you ? (
                      <Button variant="ghost" size="sm" onClick={() => setConfirming({ kind: "leave" })}>Leave</Button>
                    ) : editable ? (
                      <Button variant="ghost" size="sm" onClick={() => setConfirming({ kind: "remove", email: row.email, name: row.name })}>Remove</Button>
                    ) : null}
                  </TableCell>
                </TableRow>
              );
            })}
          </TableBody>
        </Table>
      </SettingsSection>
      {data.invites.length > 0 ? (
        <SettingsSection id="invites" title="Pending invites">
          <Table className="table-fixed">
            <TableHeader>
              <TableRow>
                <TableHead>For</TableHead>
                <TableHead className={WIDTHS.role}>Role</TableHead>
                <TableHead className={WIDTHS.date}>Expires</TableHead>
                <TableHead className={WIDTHS.actions}><span className="sr-only">Actions</span></TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {data.invites.map((invite) => (
                <TableRow key={invite.id}>
                  <TableCell>
                    <div className="flex min-w-0 flex-col">
                      <span className="truncate" title={invite.email ?? undefined}>{invite.email ?? <span className="text-muted-foreground">Anyone with the link</span>}</span>
                      <span className="text-xs whitespace-normal text-muted-foreground">
                        From {invite.invited_by}<span className="sm:hidden"> · expires {formatDate(invite.expires_at)}</span>
                      </span>
                    </div>
                  </TableCell>
                  <TableCell>{ROLE_LABELS[invite.role]}</TableCell>
                  <TableCell className="hidden text-muted-foreground sm:table-cell">{formatDate(invite.expires_at)}</TableCell>
                  <TableCell className="text-right">
                    {manager ? (
                      <Button variant="ghost" size="sm" onClick={() => revoke.mutate(invite.id)}>Revoke</Button>
                    ) : null}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </SettingsSection>
      ) : null}
      {inviting ? <InviteDialog organizationName={organizationName} mail={data.mail} invite={(input) => inviteMemberServerFn({ data: { organizationSlug, ...input } })} onInvited={refresh} onOpenChange={setInviting} /> : null}
      <AlertDialog
        open={confirming !== null}
        onOpenChange={(open) => {
          if (open) return;
          setConfirming(null);
          leave.reset();
        }}
      >
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{confirming?.kind === "remove" ? `Remove ${confirming.name}?` : `Leave ${organizationName}?`}</AlertDialogTitle>
            <AlertDialogDescription>
              {confirming?.kind === "remove"
                ? "They lose dashboard access, their organization tokens stop working, and their devices lose access to its servers."
                : "You lose dashboard access, your organization tokens stop working, and your devices lose access to its servers. Someone must invite you to come back."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          {confirming?.kind === "leave" && leave.error ? (
            <p role="alert" className="text-sm text-destructive">{toErrorMessage(leave.error, "Couldn't leave the organization.")}</p>
          ) : null}
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              variant="destructive"
              disabled={leave.isPending}
              onClick={() => {
                if (confirming?.kind !== "remove") return leave.mutate();
                remove.mutate(confirming.email);
                setConfirming(null);
              }}
            >
              {leave.isPending ? <Spinner data-icon="inline-start" /> : null}
              {confirming?.kind === "remove" ? "Remove" : "Leave"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

type InviteInput = { email: string | null; role: InviteRole };
type Invited = Awaited<ReturnType<typeof inviteMemberServerFn>>;

export function InviteDialog({ organizationName, mail, invite: send, onInvited, onOpenChange }: {
  organizationName: string;
  mail: boolean;
  invite: (input: InviteInput) => Promise<Invited>;
  onInvited: () => void;
  onOpenChange: (open: boolean) => void;
}) {
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<InviteRole>("member");
  const invite = useMutation({
    mutationFn: () => send({ email: email.trim() || null, role }),
    onSuccess: onInvited,
  });

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Invite to {organizationName}</DialogTitle>
          <DialogDescription>
            {invite.data
              ? invite.data.message
              : !email.trim()
                ? "Anyone with the link can join, once."
                : mail
                  ? "Ployz emails the link to this address. Anyone with the link can join, once."
                  : "The email only labels the invite. Anyone with the link can join, once."}
          </DialogDescription>
        </DialogHeader>
        {invite.data ? (
          <div className="flex items-center gap-2">
            <Input readOnly aria-label="Invite link" className="ph-no-capture font-mono text-xs" value={invite.data.link} onFocus={(event) => event.target.select()} />
            <CopyButton value={invite.data.link} label="Copy link" />
          </div>
        ) : (
          <form
            id="invite-form"
            onSubmit={(event) => {
              event.preventDefault();
              if (!invite.isPending) invite.mutate();
            }}
          >
            <FieldGroup>
              <Field>
                <FieldLabel htmlFor="invite-email">Email (optional)</FieldLabel>
                <Input id="invite-email" type="email" autoComplete="off" placeholder="bob@example.com" value={email} onChange={(event) => setEmail(event.target.value)} autoFocus />
              </Field>
              <Field>
                <FieldLabel htmlFor="invite-role">Role</FieldLabel>
                <NativeSelect id="invite-role" value={role} onChange={(event) => setRole(event.target.value === "admin" ? "admin" : "member")}>
                  <NativeSelectOption value="member">Member</NativeSelectOption>
                  <NativeSelectOption value="admin">Admin</NativeSelectOption>
                </NativeSelect>
              </Field>
              {invite.error ? <p role="alert" className="text-sm text-destructive">{toErrorMessage(invite.error, "Couldn't make the invite.")}</p> : null}
            </FieldGroup>
          </form>
        )}
        <DialogFooter>
          {invite.data ? (
            <DialogClose render={<Button />}>Done</DialogClose>
          ) : (
            <>
              <DialogClose render={<Button variant="outline" />}>Cancel</DialogClose>
              <Button type="submit" form="invite-form" disabled={invite.isPending}>
                {invite.isPending ? <Spinner data-icon="inline-start" /> : null}
                Make link
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
