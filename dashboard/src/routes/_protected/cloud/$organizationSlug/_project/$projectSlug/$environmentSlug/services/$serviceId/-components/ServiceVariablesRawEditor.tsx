import { useReducer, useRef } from "react";
import type { Persistable } from "#/collections/query-collection";
import { Result } from "effect";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import type { ReferenceTarget } from "#/modules/variables/variable-autocomplete";
import type { VariableRecord } from "#/modules/variables/variables";
import {
  diffVariables,
  findSealedVariableNameCollisions,
  findDuplicateEnvKeys,
  findDuplicateJsonKeys,
  getSealedVariableCollisionMessage,
  parseEnv,
  parseJson,
  type RawEditorDiff,
  type RawEditorParseError,
  serializeEntriesToEnv,
  serializeEntriesToJson,
  serializeVariablesToEnv,
  serializeVariablesToJson,
  type ParsedEntry,
} from "#/modules/variables/variable-raw-editor";
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
  onApply,
  variables,
  valueTargets = EMPTY_REFERENCE_TARGETS,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Saves the editor's creates, updates and deletes: optimistic, rolled back and toasted on failure. */
  onApply: (diff: RawEditorDiff) => Persistable;
  variables: VariableRecord[];
  valueTargets?: ReferenceTarget[];
}) {
  const sealedVariables = variables.filter(
    (variable) => variable.value.type === "sealed",
  );
  const editableVariables = variables.filter(
    (variable) => variable.value.type === "plain",
  );
  const sealedCount = sealedVariables.length;

  const [editor, dispatchEditor] = useReducer(
    rawEditorReducer,
    initialRawEditorState,
  );
  const prevOpenRef = useRef(open);
  const isEditorInitializedRef = useRef(false);
  /** What was typed when the Store refused it, with its reason: the editor reopens on it. */
  const refusedRef = useRef<Pick<RawEditorState, "mode" | "envText" | "jsonText" | "submitError"> | null>(null);

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
      if (refusedRef.current) dispatchEditor({ type: "patch", patch: refusedRef.current });
      else resetEditorFromVariables();
      refusedRef.current = null;
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

  function handleSubmit() {
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

    const typed = { mode: editor.mode, envText: editor.envText, jsonText: editor.jsonText };
    // Optimistic: a refusal reopens the editor on what was typed, with the Store's reason over it.
    onApply(diff).isPersisted.promise.catch((error) => {
      refusedRef.current = { ...typed, submitError: error instanceof Error ? error.message : "The variables couldn’t be saved." };
      onOpenChange(true);
    });
    onOpenChange(false);
  }

  function handleOpenChange(nextOpen: boolean) {
    if (!nextOpen) {
      dispatchEditor({
        type: "patch",
        patch: { parseError: null, submitError: null },
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
          text={editor.mode === "env" ? editor.envText : editor.jsonText}
          mode={editor.mode}
          onCancel={() => handleOpenChange(false)}
          onSubmit={handleSubmit}
        />
      </DialogContent>
    </Dialog>
  );
}
