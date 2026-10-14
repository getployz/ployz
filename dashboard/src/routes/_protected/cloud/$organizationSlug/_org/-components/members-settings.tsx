import { useState } from "react";
import { useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
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

const formatDate = (iso: string) => new Date(iso).toLocaleDateString(undefined, { dateStyle: "medium" });

type Pending = { kind: "remove"; email: string; name: string } | { kind: "leave" } | null;

export function MembersSettings({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const navigate = useNavigate();
  const { data } = useSuspenseQuery(organizationMembersQueryOptions(organizationSlug));
  const [inviting, setInviting] = useState(false);
  const [confirming, setConfirming] = useState<Pending>(null);
  const refresh = () => queryClient.invalidateQueries({ queryKey: membersKey(organizationSlug) });
  const failed = (fallback: string) => (cause: unknown) => toast.error(toErrorMessage(cause, fallback));

  const setRole = useMutation({
    mutationFn: (input: { member: string; role: Role }) => setMemberRoleServerFn({ data: { organizationSlug, ...input } }),
    onSettled: refresh,
    onError: failed("Couldn't change the role."),
  });
  const remove = useMutation({
    mutationFn: (member: string) => removeMemberServerFn({ data: { organizationSlug, member } }),
    onSuccess: () => setConfirming(null),
    onSettled: refresh,
    onError: failed("Couldn't remove the member."),
  });
  const leave = useMutation({
    mutationFn: () => leaveOrganizationServerFn({ data: { organizationSlug } }),
    // The session acts in another Organization now, or none.
    onSuccess: () => navigate({ to: "/cloud", replace: true, reloadDocument: true }),
    onError: failed("Couldn't leave the organization."),
  });
  const revoke = useMutation({
    mutationFn: (invite: string) => revokeInviteServerFn({ data: { organizationSlug, invite } }),
    onSettled: refresh,
    onError: failed("Couldn't revoke the invite."),
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
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Name</TableHead>
              <TableHead>Role</TableHead>
              <TableHead>Joined</TableHead>
              <TableHead><span className="sr-only">Actions</span></TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {data.members.map((row) => {
              const editable = manager && !row.you && (yourRole === "owner" || row.role !== "owner");
              return (
                <TableRow key={row.email}>
                  <TableCell>
                    <div className="flex flex-col">
                      <span>{row.name}{row.you ? <span className="text-muted-foreground"> (you)</span> : null}</span>
                      <span className="text-xs text-muted-foreground">{row.email}</span>
                    </div>
                  </TableCell>
                  <TableCell>
                    {editable ? (
                      <NativeSelect
                        size="sm"
                        aria-label={`Role of ${row.name}`}
                        value={row.role}
                        disabled={setRole.isPending}
                        onChange={(event) => {
                          const role = roles.find((candidate) => candidate === event.target.value);
                          if (role) setRole.mutate({ member: row.email, role });
                        }}
                      >
                        {roles.map((role) => <NativeSelectOption key={role} value={role}>{ROLE_LABELS[role]}</NativeSelectOption>)}
                      </NativeSelect>
                    ) : ROLE_LABELS[row.role]}
                  </TableCell>
                  <TableCell className="text-muted-foreground">{formatDate(row.joined_at)}</TableCell>
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
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>For</TableHead>
                <TableHead>Role</TableHead>
                <TableHead>Sent by</TableHead>
                <TableHead>Expires</TableHead>
                <TableHead><span className="sr-only">Actions</span></TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {data.invites.map((invite) => (
                <TableRow key={invite.id}>
                  <TableCell>{invite.email ?? <span className="text-muted-foreground">Anyone with the link</span>}</TableCell>
                  <TableCell>{ROLE_LABELS[invite.role]}</TableCell>
                  <TableCell>{invite.invited_by}</TableCell>
                  <TableCell className="text-muted-foreground">{formatDate(invite.expires_at)}</TableCell>
                  <TableCell className="text-right">
                    {manager ? (
                      <Button variant="ghost" size="sm" disabled={revoke.isPending} onClick={() => revoke.mutate(invite.id)}>Revoke</Button>
                    ) : null}
                  </TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        </SettingsSection>
      ) : null}
      {inviting ? <InviteDialog organizationSlug={organizationSlug} onInvited={refresh} onOpenChange={setInviting} /> : null}
      <AlertDialog open={confirming !== null} onOpenChange={(open) => !open && setConfirming(null)}>
        <AlertDialogContent>
          <AlertDialogHeader>
            <AlertDialogTitle>{confirming?.kind === "remove" ? `Remove ${confirming.name}?` : `Leave ${organizationSlug}?`}</AlertDialogTitle>
            <AlertDialogDescription>
              {confirming?.kind === "remove"
                ? "Their organization tokens stop working and their devices lose access to its servers."
                : "Your organization tokens stop working and your devices lose access to its servers. Someone must invite you to come back."}
            </AlertDialogDescription>
          </AlertDialogHeader>
          <AlertDialogFooter>
            <AlertDialogCancel>Cancel</AlertDialogCancel>
            <AlertDialogAction
              variant="destructive"
              disabled={remove.isPending || leave.isPending}
              onClick={() => confirming?.kind === "remove" ? remove.mutate(confirming.email) : leave.mutate()}
            >
              {remove.isPending || leave.isPending ? <Spinner data-icon="inline-start" /> : null}
              {confirming?.kind === "remove" ? "Remove" : "Leave"}
            </AlertDialogAction>
          </AlertDialogFooter>
        </AlertDialogContent>
      </AlertDialog>
    </div>
  );
}

function InviteDialog({ organizationSlug, onInvited, onOpenChange }: {
  organizationSlug: string;
  onInvited: () => void;
  onOpenChange: (open: boolean) => void;
}) {
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<InviteRole>("member");
  const invite = useMutation({
    mutationFn: () => inviteMemberServerFn({ data: { organizationSlug, email: email.trim() || null, role } }),
    onSuccess: onInvited,
  });

  return (
    <Dialog open onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Invite to {organizationSlug}</DialogTitle>
          <DialogDescription>
            {invite.data ? "Send this link to the person you're inviting. It works once and lasts 7 days." : "Anyone with the link can join, once."}
          </DialogDescription>
        </DialogHeader>
        {invite.data ? (
          <div className="flex items-center gap-2">
            <Input readOnly aria-label="Invite link" className="font-mono text-xs" value={invite.data.link} onFocus={(event) => event.target.select()} />
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
