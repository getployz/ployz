import { useState } from "react";
import { useMutation, useQueryClient, useSuspenseQuery } from "@tanstack/react-query";
import { useBlocker } from "@tanstack/react-router";
import { KeyRoundIcon } from "lucide-react";
import { toast } from "sonner";
import { ConfirmDialog } from "#/components/confirm-dialog";
import { CopyButton } from "#/components/copy-button";
import { SettingsEmpty, SettingsList, SettingsListItem } from "#/components/settings/settings-list";
import { SettingsAddButton, SettingsFoot, SettingsSection } from "#/components/settings/settings-section";
import { Button } from "#/components/ui/button";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "#/components/ui/dialog";
import { Field, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { Spinner } from "#/components/ui/spinner";
import { useExclusiveMutation } from "#/hooks/use-exclusive-mutation";
import { toErrorMessage } from "#/lib/error-message";
import { projectsQuery, requireView, useStoreView } from "#/modules/config-store/store-view.queries";
import { createTokenServerFn, revokeTokenServerFn } from "#/modules/identity/organization-token.functions";
import { organizationTokensQueryOptions, tokensKey } from "#/modules/identity/organization-token.queries";
import type { TokenPreset } from "#/modules/identity/token-grant";

const formatDate = (iso: string) => new Date(iso).toLocaleDateString(undefined, { dateStyle: "medium" });

export function TokensSettings({ organizationSlug }: { organizationSlug: string }) {
  const queryClient = useQueryClient();
  const { data: tokens } = useSuspenseQuery(organizationTokensQueryOptions(organizationSlug));
  const [making, setMaking] = useState(false);
  const refresh = () => queryClient.invalidateQueries({ queryKey: tokensKey(organizationSlug) });
  const revoke = useMutation({
    mutationFn: (id: string) => revokeTokenServerFn({ data: { organizationSlug, id } }),
    onError: (cause) => toast.error(toErrorMessage(cause, "Couldn't revoke the token.")),
    onSettled: refresh,
  });

  return (
    <SettingsSection id="tokens" title="Tokens">
      {tokens.length > 0 ? (
        <SettingsList label="Tokens">
          {tokens.map((token) => (
            <TokenItem key={token.id} token={token} revoking={revoke.isPending && revoke.variables === token.id} onRevoke={() => revoke.mutate(token.id)} />
          ))}
        </SettingsList>
      ) : (
        <SettingsEmpty title="No tokens">A token lets CI or a script act for you.</SettingsEmpty>
      )}
      <SettingsFoot>
        <SettingsAddButton onClick={() => setMaking(true)}>New token</SettingsAddButton>
      </SettingsFoot>
      {making ? <NewTokenDialog organizationSlug={organizationSlug} onMade={refresh} onOpenChange={setMaking} /> : null}
    </SettingsSection>
  );
}

type TokenOwner = { readonly name: string; readonly email: string; readonly you: boolean };

export function TokenItem({ token, revoking, onRevoke }: {
  token: { name: string; scope: string; expires_at: string; expired: boolean; owner: TokenOwner };
  revoking: boolean;
  onRevoke: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const owner = token.owner.name || token.owner.email;
  const by = token.owner.you || token.scope === `Full as ${owner}` ? "" : ` · by ${owner}`;
  return (
    <SettingsListItem
      media={<KeyRoundIcon />}
      label={token.name}
      hint={`${token.scope}${by} · ${token.expired ? "Expired" : "Expires"} ${formatDate(token.expires_at)}`}
      commit="confirm"
      actions={(
        <>
          <Button variant="ghost" size="sm" disabled={revoking} onClick={() => setConfirming(true)}>
            Revoke
          </Button>
          <ConfirmDialog
            open={confirming}
            onOpenChange={setConfirming}
            title={`Revoke ${token.name}?`}
            description="Anything using it stops working."
            actionLabel="Revoke"
            variant="destructive"
            onConfirm={() => {
              setConfirming(false);
              onRevoke();
            }}
          />
        </>
      )}
    />
  );
}

type Choice = TokenPreset["preset"];

const CHOICES: ReadonlyArray<{ value: Choice; label: string }> = [
  { value: "deploy", label: "Deploy" },
  { value: "read", label: "Read only" },
  { value: "full", label: "Full as you" },
];

function NewTokenDialog({ organizationSlug, onMade, onOpenChange }: {
  organizationSlug: string;
  onMade: () => void;
  onOpenChange: (open: boolean) => void;
}) {
  const environments = [...new Set(requireView(useStoreView(organizationSlug, projectsQuery())).projects.flatMap((project) => project.environments))].sort();
  const [name, setName] = useState("");
  const [choice, setChoice] = useState<Choice>(environments.length > 0 ? "deploy" : "read");
  const [picked, setEnvironment] = useState<string | null>(null);
  const environment = picked !== null && environments.includes(picked) ? picked : environments.includes("staging") ? "staging" : (environments[0] ?? "");
  const preset: TokenPreset = choice === "deploy" ? { preset: "deploy", environment } : { preset: choice };
  const make = useExclusiveMutation({
    mutationFn: () => createTokenServerFn({ data: { organizationSlug, name: name.trim(), expiresInDays: 90, preset } }),
    onSuccess: onMade,
  });
  const ready = name.trim() !== "" && (choice !== "deploy" || environment !== "");
  useBlocker({ shouldBlockFn: () => true, disabled: !make.holding });

  return (
    <Dialog open onOpenChange={(open) => make.holding || onOpenChange(open)}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>New token</DialogTitle>
          <DialogDescription>
            {make.data ? "Copy it now. Ployz shows it only once." : "It acts as you, narrowed to what you pick, for 90 days."}
          </DialogDescription>
        </DialogHeader>
        {make.data ? (
          <div className="flex items-center gap-2">
            <Input readOnly aria-label="Token" className="ph-no-capture font-mono text-xs" value={make.data.secret} onFocus={(event) => event.target.select()} />
            <CopyButton value={make.data.secret} label="Copy token" />
          </div>
        ) : (
          <form
            id="token-form"
            onSubmit={(event) => {
              event.preventDefault();
              if (ready) make.mutate();
            }}
          >
            <FieldGroup>
              <Field>
                <FieldLabel htmlFor="token-name">Name</FieldLabel>
                <Input id="token-name" autoComplete="off" maxLength={64} placeholder="ci" value={name} onChange={(event) => setName(event.target.value)} autoFocus />
              </Field>
              <Field>
                <FieldLabel htmlFor="token-preset">Access</FieldLabel>
                <NativeSelect id="token-preset" value={choice} onChange={(event) => setChoice(CHOICES.find((option) => option.value === event.target.value)?.value ?? "deploy")}>
                  {CHOICES.map((option) => (
                    <NativeSelectOption key={option.value} value={option.value} disabled={option.value === "deploy" && environments.length === 0}>{option.label}</NativeSelectOption>
                  ))}
                </NativeSelect>
              </Field>
              {choice === "deploy" ? (
                <Field>
                  <FieldLabel htmlFor="token-environment">Environment</FieldLabel>
                  <NativeSelect id="token-environment" value={environment} onChange={(event) => setEnvironment(event.target.value)}>
                    {environments.map((candidate) => <NativeSelectOption key={candidate} value={candidate}>{candidate}</NativeSelectOption>)}
                  </NativeSelect>
                </Field>
              ) : null}
              {make.stalled ? (
                <div role="alert" className="text-sm text-destructive">
                  <p>Ployz hasn't answered. Retry or cancel.</p>
                  <p>If it made the token, it shows in Tokens, where you can revoke it.</p>
                </div>
              ) : null}
              {make.error ? <p role="alert" className="text-sm text-destructive">{toErrorMessage(make.error, "Couldn't make the token.")}</p> : null}
            </FieldGroup>
          </form>
        )}
        <DialogFooter>
          {make.data ? (
            <DialogClose render={<Button />}>Done</DialogClose>
          ) : (
            <>
              <DialogClose render={<Button variant="outline" disabled={make.holding} />}>Cancel</DialogClose>
              <Button type="submit" form="token-form" disabled={!ready || make.holding}>
                {make.holding ? <Spinner data-icon="inline-start" /> : null}
                {make.stalled ? "Retry" : "Make token"}
              </Button>
            </>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
