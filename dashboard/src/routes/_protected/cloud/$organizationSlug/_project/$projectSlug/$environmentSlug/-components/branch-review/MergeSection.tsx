import { useState } from "react";
import { useParams } from "@tanstack/react-router";
import type { BranchOption } from "@ployz/sdk/config";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Field, FieldContent, FieldDescription, FieldError, FieldLabel } from "#/components/ui/field";
import { Input } from "#/components/ui/input";
import { ItemGroup } from "#/components/ui/item";
import { NativeSelect, NativeSelectOption } from "#/components/ui/native-select";
import { Spinner } from "#/components/ui/spinner";
import { Switch } from "#/components/ui/switch";
import { useMergeBranch } from "#/modules/branches/branch-commands";
import { useBranchUnsettled } from "#/modules/branches/branch.collection";
import { presentRow } from "#/modules/branches/branch-review";
import type { BranchReviewView } from "#/modules/branches/use-branch-review";
import { useWorkspace } from "#/modules/environment-design/workspace.queries";
import { ENVIRONMENT_ROUTE_FROM } from "../environment-route-paths";
import { ReviewSection } from "./BranchReviewPanel";
import { ChangeRowItem } from "./ChangeRowItem";

type Pick = { ticked: boolean; option?: BranchOption; value: string };

/**
 * What would merge into the Destination, change by change: the Branch's Working State against the Destination's, over the
 * base. Each change has a tick and each variable a value choice. Merge stages the ticked ones in the Destination, and is
 * open only while the Branch runs exactly its Working State.
 */
export function MergeSection({ review, branch }: { review: BranchReviewView; branch: { id: string; name: string } }) {
  const params = useParams({ from: ENVIRONMENT_ROUTE_FROM });
  const destination = review.parent.name;
  const unsettled = useBranchUnsettled(params.organizationSlug, branch.id);
  const isDefault = useWorkspace(params.organizationSlug).projects.some((project) => project.defaultEnvironmentId === branch.id);
  const merge = useMergeBranch({ organizationSlug: params.organizationSlug, projectSlug: params.projectSlug, branchName: branch.name, destination: review.parent });
  const [edits, setEdits] = useState<Record<string, Partial<Pick>>>({});
  const [thenClose, setThenClose] = useState(true);

  const picks = review.merge.map((row) => {
    const choice = row.role === "move" ? row.choice : undefined;
    const pick: Pick = { ticked: true, option: choice?.default, value: "", ...edits[row.key] };
    return { row, choice, pick, presented: presentRow(row, review.nameOf) };
  });
  const edit = (key: string, change: Partial<Pick>) => setEdits((current) => ({ ...current, [key]: { ...current[key], ...change } }));
  const ticked = picks.filter(({ pick }) => pick.ticked);
  const missing = ticked.find(({ choice, pick }) => pick.option === "new" && !choice?.secret && !pick.value);
  const closes = !review.kept && !isDefault && thenClose;
  const blocked = unsettled
    ?? (ticked.length === 0 ? "Tick a change to merge."
    : missing ? `Enter a new value for ${missing.presented.label}.`
    : null);

  function submit() {
    merge.mutate({
      branchEnvironmentId: branch.id, review: review.mergeReview, thenClose: closes,
      picks: ticked.map(({ row, pick }) => ({
        key: row.key, option: pick.option, value: pick.option === "new" ? pick.value : "",
      })),
    });
  }

  return (
    <ReviewSection title={`Merge into ${destination}`} count={review.merge.length}>
      {review.merge.length ? (
        <>
          <ItemGroup className="gap-1">
            {picks.map(({ row, choice, pick, presented }) => (
              <div key={row.key} className="flex flex-col gap-1">
                <ChangeRowItem row={presented} conflict={row.role === "move" && row.conflict ? destination : undefined}>
                  <Checkbox checked={pick.ticked} onCheckedChange={(checked) => edit(row.key, { ticked: checked === true })}
                    aria-label={`Merge ${presented.node}${presented.label ? ` · ${presented.label}` : ""}`} />
                </ChangeRowItem>
                {choice && pick.ticked ? (
                  <div className="flex flex-wrap gap-1">
                    <NativeSelect aria-label={`Value of ${presented.label}`} value={pick.option}
                      // SAFETY: the options are exactly choice.options.
                      onChange={(event) => edit(row.key, { option: event.target.value as BranchOption })}>
                      {choice.options.map((option) => (
                        <NativeSelectOption key={option} value={option}>
                          {option === "from" ? `${branch.name}'s value` : option === "parent" ? `${destination}'s value`
                            : option === "new" ? "A new value" : "Leave it out"}
                        </NativeSelectOption>
                      ))}
                    </NativeSelect>
                    {pick.option === "new" ? (
                      <Input className="min-w-48 flex-1" type={choice.secret ? "password" : "text"} autoComplete="off" value={pick.value}
                        aria-label={`New value of ${presented.label}`}
                        placeholder={choice.secret ? `${destination}'s value, or empty for later` : `${destination}'s value`}
                        onChange={(event) => edit(row.key, { value: event.target.value })} />
                    ) : null}
                  </div>
                ) : null}
              </div>
            ))}
          </ItemGroup>
          {review.kept ? null : (
            <FieldLabel htmlFor="merge-then-close">
              <Field orientation="horizontal" data-disabled={isDefault || undefined}>
                <FieldContent>
                  <span>Then close {branch.name}</span>
                  <FieldDescription>
                    {isDefault ? `${branch.name} is the Default Environment, so it stays open.` : "Removes its services and their data once merged."}
                  </FieldDescription>
                </FieldContent>
                <Switch id="merge-then-close" checked={closes} disabled={isDefault} onCheckedChange={setThenClose} />
              </Field>
            </FieldLabel>
          )}
          <div className="flex flex-col gap-2">
            <Button className="self-start" disabled={blocked !== null || merge.isPending} onClick={submit}>
              {merge.isPending ? <Spinner data-icon="inline-start" /> : null}Merge into {destination}
            </Button>
            {blocked ? <FieldDescription>{blocked}</FieldDescription>
              : <FieldDescription>Stages the ticked changes in {destination}. Its own Review → Deploy ships them.</FieldDescription>}
            {merge.isError ? <FieldError>{merge.error.message}</FieldError> : null}
          </div>
        </>
      ) : <p className="text-sm text-muted-foreground">Nothing here that {destination} doesn't have.</p>}
    </ReviewSection>
  );
}
