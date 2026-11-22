import { useState, type ReactNode } from "react";
import { PlusIcon } from "lucide-react";
import { Button } from "#/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "#/components/ui/dialog";
import { Separator } from "#/components/ui/separator";
import { VariableAddForm } from "./variable-add-form";
import type { SealedControls, VariableAddInput } from "./variable-types";
import { VariableRow } from "./variable-row";
import type { Write } from "#/modules/config-store/store-write";
import type { ReferenceTarget } from "#/modules/variables/variable-autocomplete";
import type { VariableWriter } from "#/modules/variables/variables";
import type { PlainVariableRecord } from "#/modules/variables/variables";
import type { VariableRecord } from "#/modules/variables/variables";

export function VariablesPanel({
  variables,
  collection,
  countNoun,
  onCreateVariable,
  onSealVariable,
  onSetExported,
  onNeverSync,
  onRename,
  referrersOf,
  sealing,
  headerActions,
  renderAfterList,
  emptyState,
  valueTargets,
  serviceNames,
}: {
  variables: VariableRecord[];
  collection: VariableWriter;
  /** Reference targets for the value `${{ }}` autocomplete (add form + rows). */
  valueTargets?: ReferenceTarget[];
  /** Every Service of the Environment, which a value's `${{ service.KEY }}` may name; none: unchecked. */
  serviceNames?: readonly string[];
  /** Singular noun for the count heading, e.g. "Variable" or "Service Variable". */
  countNoun: string;
  /** Saves in the background; a refusal reopens the form with what was typed. */
  onCreateVariable: (input: VariableAddInput) => Write;
  /** Seals a plain variable's value, or sets a sealed one's, in the background. */
  onSealVariable: (variable: PlainVariableRecord) => Write;
  onNeverSync: (variable: VariableRecord, marked: boolean) => void;
  /** Each row's menu renames it; with `updateReferences`, the variables using it switch too. */
  onRename: (variable: VariableRecord, to: string, updateReferences: boolean) => void;
  /** The variables that reference one, as `SERVICE.KEY`, which its rename switches to the new name. */
  referrersOf: (variable: VariableRecord) => readonly string[];
  onSetExported: (variable: VariableRecord, exported: boolean) => void;
  sealing: SealedControls;
  /** Buttons next to "New Variable". */
  headerActions: ReactNode;
  renderAfterList: () => ReactNode;
  emptyState: ReactNode;
}) {
  const [isAdding, setIsAdding] = useState(false);
  const [draft, setDraft] = useState<VariableAddInput>();

  const showEmptyState = variables.length === 0;

  return (
    <div className="flex flex-col gap-6">
      <Dialog open={isAdding} onOpenChange={setIsAdding}>
        <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
          <h2 className="font-medium">
            {variables.length} {countNoun}
            {variables.length === 1 ? "" : "s"}
          </h2>
          <div className="flex flex-wrap items-center gap-2">
            {headerActions}
            <DialogTrigger
              render={<Button type="button" variant="outline" />}
            >
              <PlusIcon data-icon="inline-start" />
              New Variable
            </DialogTrigger>
          </div>
        </div>
        <DialogContent className="max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>New Variable</DialogTitle>
          </DialogHeader>
          <VariableAddForm
            variables={variables}
            onCreateVariable={(input) => {
              setDraft(undefined);
              onCreateVariable(input).catch(() => {
                setDraft(input);
                setIsAdding(true);
              });
            }}
            initial={draft}
            onCancel={() => setIsAdding(false)}
            sealing={sealing}
            valueTargets={valueTargets}
            serviceNames={serviceNames}
          />
        </DialogContent>
      </Dialog>

      <Separator />

      <div className="flex flex-col gap-6">
        {variables.length > 0 ? (
          <div className="flex flex-col">
            {variables.map((variable) => (
              <VariableRow
                key={variable.id}
                variable={variable}
                collection={collection}
                valueTargets={valueTargets}
                onSealVariable={onSealVariable}
                onSetExported={onSetExported}
                onNeverSync={onNeverSync}
                onRename={onRename}
                referrers={referrersOf(variable)}
              />
            ))}
          </div>
        ) : null}

        {showEmptyState ? emptyState : null}

        {renderAfterList()}
      </div>
    </div>
  );
}
