import { useReducer, useRef } from "react";
import { eq, useLiveSuspenseQuery } from "@tanstack/react-db";
import { Result } from "effect";
import { toast } from "sonner";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import { parseLiveQueryRow } from "#/lib/tanstack-db";
import type { ReferenceTarget } from "#/modules/environment-design/variable-autocomplete";
import { variableSelectSchema } from "#/modules/environment-design/variables";
import type { OrganizationVariablesCollection } from "#/modules/environment-design/variable-collections";
import { useApplyRawVariablesAction } from "#/modules/environment-design/variable-mutation-actions";
import {
  diffVariables,
  findSealedVariableNameCollisions,
  findDuplicateEnvKeys,
  findDuplicateJsonKeys,
  getSealedVariableCollisionMessage,
  parseEnv,
  parseJson,
  type RawEditorParseError,
  serializeEntriesToEnv,
  serializeEntriesToJson,
  serializeVariablesToEnv,
  serializeVariablesToJson,
  type ParsedEntry,
} from "#/modules/environment-design/variable-raw-editor";
import { ServiceVariablesRawEditorAlerts } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesRawEditorAlerts";
import { ServiceVariablesRawEditorFooter } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesRawEditorFooter";
import { ServiceVariablesRawEditorTabs } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesRawEditorTabs";
import type { RawEditorMode } from "#/routes/_protected/cloud/$organizationSlug/_project/$projectSlug/$environmentSlug/services/$serviceId/-components/ServiceVariablesRawEditorTypes";

const EMPTY_REFERENCE_TARGETS: ReferenceTarget[] = [];

type RawEditorState = {
  mode: RawEditorMode;
  envText: string;
  jsonText: string;
  parseError: string | null;
  submitError: string | null;
  copied: boolean;
  isSubmitting: boolean;
};

type RawEditorAction =
  | { type: "patch"; patch: Partial<RawEditorState> }
  | { type: "reset"; envText: string; jsonText: string };

const initialRawEditorState: RawEditorState = {
  mode: "env",
  envText: "",
  jsonText: "",
  parseError: null,
  submitError: null,
  copied: false,
  isSubmitting: false,
};

function rawEditorReducer(
  state: RawEditorState,
  action: RawEditorAction,
): RawEditorState {
  switch (action.type) {
    case "patch":
      return { ...state, ...action.patch };
    case "reset":
      return {
        ...initialRawEditorState,
        envText: action.envText,
        jsonText: action.jsonText,
      };
  }
}

function parseForMode(
  text: string,
  mode: RawEditorMode,
): Result.Result<ParsedEntry[], RawEditorParseError> {
  return mode === "env" ? parseEnv(text) : parseJson(text);
}

export function ServiceVariablesRawEditor({
  open,
  onOpenChange,
  organizationSlug,
  environmentId,
  serviceId,
  collection,
  valueTargets = EMPTY_REFERENCE_TARGETS,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  organizationSlug: string;
  environmentId: string;
  serviceId: string;
  collection: OrganizationVariablesCollection;
  valueTargets?: ReferenceTarget[];
}) {
  const { data: rawVariables } = useLiveSuspenseQuery({
    query: (q) =>
      q
        .from({ variable: collection })
        .where(({ variable }) => eq(variable.serviceId, serviceId))
        .orderBy(({ variable }) => variable.key)
        .select(({ variable }) => variable),
  });
  const variables = rawVariables.map((row) =>
    parseLiveQueryRow(variableSelectSchema, row),
  );
  const sealedVariables = variables.filter(
    (variable) => variable.value.type === "sealed",
  );
  const editableVariables = variables.filter(
    (variable) => variable.value.type === "plain",
  );
  const sealedCount = sealedVariables.length;

  const applyRawVariables = useApplyRawVariablesAction({
    collection,
    organizationSlug,
    environmentId,
    serviceId,
  });

  const [editor, dispatchEditor] = useReducer(
    rawEditorReducer,
    initialRawEditorState,
  );
  const prevOpenRef = useRef(open);
  const isEditorInitializedRef = useRef(false);

  function resetEditorFromVariables() {
    dispatchEditor({
      type: "reset",
      envText: serializeVariablesToEnv(editableVariables),
      jsonText: serializeVariablesToJson(editableVariables),
    });
  }

  if (open !== prevOpenRef.current) {
    prevOpenRef.current = open;
    if (open) {
      resetEditorFromVariables();
      isEditorInitializedRef.current = true;
    } else {
      isEditorInitializedRef.current = false;
      dispatchEditor({
        type: "patch",
        patch: { parseError: null, submitError: null, mode: "env" },
      });
    }
  }

  if (open && !isEditorInitializedRef.current) {
    resetEditorFromVariables();
    isEditorInitializedRef.current = true;
  }

  function handleModeChange(nextMode: RawEditorMode) {
    if (nextMode === editor.mode) return;
    const result = parseForMode(
      editor.mode === "env" ? editor.envText : editor.jsonText,
      editor.mode,
    );
    if (Result.isFailure(result)) {
      dispatchEditor({
        type: "patch",
        patch: { parseError: result.failure.message },
      });
      return;
    }
    if (nextMode === "env") {
      dispatchEditor({
        type: "patch",
        patch: {
          envText: serializeEntriesToEnv(result.success),
          parseError: null,
          submitError: null,
          mode: nextMode,
        },
      });
    } else {
      dispatchEditor({
        type: "patch",
        patch: {
          jsonText: serializeEntriesToJson(result.success),
          parseError: null,
          submitError: null,
          mode: nextMode,
        },
      });
    }
  }

  async function handleSubmit() {
    dispatchEditor({
      type: "patch",
      patch: { parseError: null, submitError: null },
    });
    const parseResult = parseForMode(
      editor.mode === "env" ? editor.envText : editor.jsonText,
      editor.mode,
    );
    if (Result.isFailure(parseResult)) {
      dispatchEditor({
        type: "patch",
        patch: { parseError: parseResult.failure.message },
      });
      return;
    }
    const entries = parseResult.success;
    const sealedCollisions = findSealedVariableNameCollisions(
      entries,
      sealedVariables,
    );
    const [sealedCollision] = sealedCollisions;
    if (sealedCollision) {
      dispatchEditor({
        type: "patch",
        patch: {
          submitError: getSealedVariableCollisionMessage(sealedCollision),
        },
      });
      return;
    }

    const diff = diffVariables(entries, editableVariables);
    if (
      diff.creates.length === 0 &&
      diff.updates.length === 0 &&
      diff.deletes.length === 0
    ) {
      onOpenChange(false);
      return;
    }

    dispatchEditor({ type: "patch", patch: { isSubmitting: true } });
    const tx = applyRawVariables(diff);
    try {
      await tx.isPersisted.promise;
      onOpenChange(false);
    } catch (error) {
      toast.error(
        error instanceof Error
          ? error.message
          : "The variables couldn’t be updated. Try again.",
      );
    } finally {
      dispatchEditor({ type: "patch", patch: { isSubmitting: false } });
    }
  }

  async function handleCopyEnv() {
    const clipboard = globalThis.navigator?.clipboard;
    if (!clipboard) {
      toast.error("Couldn’t access the clipboard");
      return;
    }
    await clipboard.writeText(editor.envText);
    dispatchEditor({ type: "patch", patch: { copied: true } });
    setTimeout(
      () => dispatchEditor({ type: "patch", patch: { copied: false } }),
      1500,
    );
  }

  function handleOpenChange(nextOpen: boolean) {
    if (!nextOpen) {
      dispatchEditor({
        type: "patch",
        patch: { parseError: null, submitError: null, copied: false },
      });
    }
    onOpenChange(nextOpen);
  }

  const duplicateKeys =
    editor.mode === "env"
      ? findDuplicateEnvKeys(editor.envText)
      : findDuplicateJsonKeys(editor.jsonText);

  return (
    <Dialog modal="trap-focus" open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="min-w-0 max-h-[calc(100dvh-2rem)] overflow-x-hidden overflow-y-auto sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Raw editor</DialogTitle>
          <DialogDescription>
            Add, edit, or delete your service variables.
          </DialogDescription>
        </DialogHeader>

        <ServiceVariablesRawEditorAlerts
          sealedCount={sealedCount}
          parseError={editor.parseError}
          submitError={editor.submitError}
          duplicateKeys={duplicateKeys}
        />

        <ServiceVariablesRawEditorTabs
          mode={editor.mode}
          envText={editor.envText}
          jsonText={editor.jsonText}
          valueTargets={valueTargets}
          onModeChange={handleModeChange}
          onEnvTextChange={(next) => {
            dispatchEditor({
              type: "patch",
              patch: {
                envText: next,
                parseError: null,
                submitError: null,
              },
            });
          }}
          onJsonTextChange={(next) => {
            dispatchEditor({
              type: "patch",
              patch: {
                jsonText: next,
                parseError: null,
                submitError: null,
              },
            });
          }}
        />

        <ServiceVariablesRawEditorFooter
          copied={editor.copied}
          isSubmitting={editor.isSubmitting}
          onCopyEnv={() => void handleCopyEnv()}
          onCancel={() => handleOpenChange(false)}
          onSubmit={() => void handleSubmit()}
        />
      </DialogContent>
    </Dialog>
  );
}
