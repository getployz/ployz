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
import {
  Empty,
  EmptyHeader,
  EmptyTitle,
} from "#/components/ui/empty";
import { Separator } from "#/components/ui/separator";
import { VariableAddForm } from "#/components/variables/variable-add-form";
import {
  VariableRow,
  type VariableMetadataPatch,
} from "#/components/variables/variable-row";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";
import type { VariableWriter } from "#/modules/environment-design/variable-collections";
import type { PlainVariableRecord } from "#/modules/environment-design/variable-mutation-actions";
import type { VariableRecord } from "#/modules/environment-design/variables";

export type VariableAddInput = {
  key: string;
  value: string;
  sealed: boolean;
  exported: boolean;
};

export function VariablesPanel({
  variables,
  collection,
  countNoun,
  onCreateVariable,
  onSealVariable,
  onUpdateMetadata,
  allowSealOnCreate = false,
  defaultExported = false,
  headerActions,
  renderBeforeList,
  renderAfterList,
  emptyState,
  variableWarnings,
  valueTargets,
}: {
  variables: VariableRecord[];
  collection: VariableWriter;
  /** Reference targets for the value `${{ }}` autocomplete (add form + rows). */
  valueTargets?: ReferenceTarget[];
  /** Singular noun for the count heading, e.g. "Variable" or "Service Variable". */
  countNoun: string;
  onCreateVariable: (input: VariableAddInput) => void;
  onSealVariable: (variable: PlainVariableRecord) => void;
  onUpdateMetadata?: (variable: VariableRecord, patch: VariableMetadataPatch) => void;
  /** Show the "Sealed" toggle in the add form (owners that support sealed-on-create). */
  allowSealOnCreate?: boolean;
  /** Whether the "Exported" toggle starts checked (true for export-first owners like Variable Groups). */
  defaultExported?: boolean;
  /** Extra buttons rendered next to "New Variable" (e.g. a raw editor). */
  headerActions?: ReactNode;
  /** Rendered above the variable rows (e.g. an alert or bindings panel). */
  renderBeforeList?: () => ReactNode;
  /** Rendered below the variable rows (e.g. managed system variables). */
  renderAfterList?: () => ReactNode;
  /** Override the default empty state. */
  emptyState?: ReactNode;
  /** Per-variable warning text rendered on editable rows. */
  variableWarnings?: Map<string, string>;
}) {
  const supportsExport = onUpdateMetadata != null;
  const [isAdding, setIsAdding] = useState(false);

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
            collection={collection}
            onCreateVariable={onCreateVariable}
            onCancel={() => setIsAdding(false)}
            allowSealOnCreate={allowSealOnCreate}
            defaultExported={defaultExported}
            supportsExport={supportsExport}
            valueTargets={valueTargets}
          />
        </DialogContent>
      </Dialog>

      <Separator />

      <div className="flex flex-col gap-6">
        {renderBeforeList?.()}

        {variables.length > 0 ? (
          <div className="flex flex-col">
            {variables.map((variable) => (
              <VariableRow
                key={variable.id}
                variable={variable}
                collection={collection}
                valueTargets={valueTargets}
                onSealVariable={onSealVariable}
                onUpdateMetadata={onUpdateMetadata}
                warning={variableWarnings?.get(variable.id)}
              />
            ))}
          </div>
        ) : null}

        {showEmptyState ? (
          emptyState ?? (
            <Empty>
              <EmptyHeader>
                <EmptyTitle>No variables yet</EmptyTitle>
              </EmptyHeader>
            </Empty>
          )
        ) : null}

        {renderAfterList?.()}
      </div>
    </div>
  );
}
