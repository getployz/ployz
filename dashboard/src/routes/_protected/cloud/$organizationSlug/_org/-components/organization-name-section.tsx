import { useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useServerFn } from "@tanstack/react-start";
import { Schema } from "effect";
import { SettingsRow, SettingsSection } from "#/components/settings/settings-section";
import { ConfirmableInput, type ConfirmStatus } from "#/components/stageable/confirmable-input";
import { toErrorMessage } from "#/lib/error-message";
import { organizationMembersQueryOptions } from "#/modules/organization/members.queries";
import { can } from "#/modules/organization/permissions";
import { OrganizationName } from "#/modules/organization/organization-state";
import { renameOrganizationServerFn } from "#/modules/organization/organization-state.functions";
import { organizationKeys, useOrganizationName } from "#/modules/organization/organization-state.queries";

const isName = Schema.is(OrganizationName);

/** The Organization's name. Its slug, and so its URLs, stay. */
export function OrganizationNameSection({ organizationSlug }: { organizationSlug: string }) {
  const name = useOrganizationName(organizationSlug);
  return (
    <SettingsSection id="name" title="Name">
      {/* Keyed so a rename saved here or elsewhere starts a fresh draft. */}
      <OrganizationNameRow key={name} organizationSlug={organizationSlug} name={name} />
    </SettingsSection>
  );
}

function OrganizationNameRow({ organizationSlug, name }: { organizationSlug: string; name: string }) {
  const [draft, setDraft] = useState(name);
  const queryClient = useQueryClient();
  const yourRole = useQuery(organizationMembersQueryOptions(organizationSlug)).data?.members.find((row) => row.you)?.role;
  const locked = yourRole === undefined || !can(yourRole, "org:settings");
  const rename = useMutation({
    mutationFn: useServerFn(renameOrganizationServerFn),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: organizationKeys.all }),
  });
  const next = draft.trim();
  const error = next === name || isName(next) ? null : next === "" ? "Enter a name." : "64 characters at most.";
  const submit = () => {
    if (next !== name && error === null) rename.mutate({ data: { organizationSlug, name: next } });
  };
  const status: ConfirmStatus | undefined = rename.isPending
    ? { phase: "saving" }
    : rename.isError ? { phase: "failed", reason: toErrorMessage(rename.error, "Could not rename"), onRetry: submit } : undefined;
  return (
    <SettingsRow
      label="Display name"
      hint="The URL stays the same."
      commit="confirm"
      control={(
        <ConfirmableInput id="organization-name" aria-label="Display name" value={draft} onValueChange={setDraft} error={error}
          disabled={locked} status={status} isDirty={draft !== name} onCancel={() => setDraft(name)} onConfirm={submit} />
      )}
    />
  );
}
