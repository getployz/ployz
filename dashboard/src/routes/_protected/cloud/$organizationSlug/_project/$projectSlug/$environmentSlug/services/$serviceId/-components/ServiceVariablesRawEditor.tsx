import { useReducer, useRef } from "react";
import { Result, Schema } from "effect";
import { LayersIcon, LockIcon } from "lucide-react";
import { CopyButton } from "#/components/copy-button";
import { Alert, AlertDescription, AlertTitle } from "#/components/ui/alert";
import { Button } from "#/components/ui/button";
import { Checkbox } from "#/components/ui/checkbox";
import { Field, FieldError, FieldGroup, FieldLabel } from "#/components/ui/field";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "#/components/ui/tabs";
import { VariableValueTextarea } from "#/components/variables/VariableValueTextarea";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "#/components/ui/dialog";
import type { Write } from "#/modules/config-store/store-write";
import { sealedValueProblem, warmSealing } from "#/modules/variables/seal-value";
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
import { toErrorMessage } from "#/lib/error-message";

const RawEditorMode = Schema.Literals(["env", "json"]);
type RawEditorMode = typeof RawEditorMode.Type;

const isRawEditorMode = Schema.is(RawEditorMode);

const EMPTY_REFERENCE_TARGETS: ReferenceTarget[] = [];

type RawEditorState = {
  mode: RawEditorMode;
  envText: string;
  jsonText: string;
  parseError: string | null;
  submitError: string | null;
  /** The new keys ticked Sealed, kept across ENV and JSON. */
  sealing: string[];
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
  sealing: [],
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

function ServiceVariablesRawEditorAlerts({
  duplicateKeys,
  parseError,
  sealedCount,
  submitError,
}: {
  duplicateKeys: string[];
  parseError: string | null;
  sealedCount: number;
  submitError: string | null;
}) {
  const shownDuplicateKeys = duplicateKeys.slice(0, 6);
  const extraDuplicateCount = duplicateKeys.length - shownDuplicateKeys.length;

  return (
    <>
      {sealedCount > 0 ? (
        <Alert>
          <LockIcon />
          <AlertDescription>
            {sealedCount} sealed variable{sealedCount === 1 ? "" : "s"} omitted
            from the editor. The value{sealedCount === 1 ? " is" : "s are"}{" "}
            retained as-is.
          </AlertDescription>
        </Alert>
      ) : null}

      {parseError ? (
        <Alert variant="destructive">
          <AlertTitle>Couldn’t parse</AlertTitle>
          <AlertDescription>{parseError}</AlertDescription>
        </Alert>
      ) : null}

      {submitError ? (
        <Alert variant="destructive">
          <AlertTitle>Couldn’t update variables</AlertTitle>
          <AlertDescription>{submitError}</AlertDescription>
        </Alert>
      ) : null}

      {duplicateKeys.length > 0 && !parseError && !submitError ? (
        <Alert>
          <LayersIcon />
          <AlertDescription>
            {duplicateKeys.length === 1
              ? "Duplicate key will be merged, keeping the last value: "
              : `${duplicateKeys.length} duplicate keys will be merged, keeping the last value: `}
            <span className="font-mono">{shownDuplicateKeys.join(", ")}</span>
            {extraDuplicateCount > 0 ? ` +${extraDuplicateCount} more` : null}
          </AlertDescription>
        </Alert>
      ) : null}
    </>
  );
}

function ServiceVariablesRawEditorTabs({
  mode,
  envText,
  jsonText,
  valueTargets,
  onEnvTextChange,
  onJsonTextChange,
  onModeChange,
}: {
  mode: RawEditorMode;
  envText: string;
  jsonText: string;
  valueTargets: ReferenceTarget[];
  onEnvTextChange: (value: string) => void;
  onJsonTextChange: (value: string) => void;
  onModeChange: (mode: RawEditorMode) => void;
}) {
  return (
    <Tabs
      className="min-w-0"
      value={mode}
      onValueChange={(value) => {
        if (isRawEditorMode(value)) onModeChange(value);
      }}
    >
      <TabsList variant="line">
        <TabsTrigger value="env">ENV</TabsTrigger>
        <TabsTrigger value="json">JSON</TabsTrigger>
      </TabsList>

      <TabsContent value="env" className="mt-3 min-w-0">
        <VariableValueTextarea
          aria-label="Service variables in ENV format"
          value={envText}
          onValueChange={onEnvTextChange}
          targets={valueTargets}
          className="h-[40dvh] min-h-40 max-h-72 max-w-full field-sizing-fixed overflow-auto font-mono text-xs"
          placeholder='KEY="value"'
        />
      </TabsContent>

      <TabsContent value="json" className="mt-3 min-w-0">
        <VariableValueTextarea
          aria-label="Service variables in JSON format"
          value={jsonText}
          onValueChange={onJsonTextChange}
          targets={valueTargets}
          className="h-[40dvh] min-h-40 max-h-72 max-w-full field-sizing-fixed overflow-auto font-mono text-xs"
          placeholder="{}"
        />
      </TabsContent>
    </Tabs>
  );
}

/**
 * While sealing is ready, or a new key is ticked: the sealed variables, which the text never holds and a save never removes, and a Sealed
 * choice for each new key.
 */
function ServiceVariablesRawEditorSealed({ sealedKeys, newKeys, sealing, problems, onSealingChange }: {
  sealedKeys: readonly string[];
  newKeys: readonly string[];
  /** The new keys ticked Sealed. */
  sealing: readonly string[];
  /** Why a ticked value can't be sealed, by key. */
  problems: ReadonlyMap<string, string>;
  onSealingChange: (key: string, sealed: boolean) => void;
}) {
  if (sealedKeys.length === 0 && newKeys.length === 0) return null;
  return (
    <div className="flex flex-col gap-3">
      {sealedKeys.length > 0 ? (
        <ul className="flex flex-col gap-1">
          {sealedKeys.map((key) => (
            <li key={key} className="flex items-center gap-1.5 text-sm text-muted-foreground">
              <LockIcon className="size-3.5" />
              <span className="font-mono text-xs">{key}</span> · Sealed
            </li>
          ))}
        </ul>
      ) : null}
      {newKeys.length > 0 ? (
        <FieldGroup className="gap-2">
          {newKeys.map((key) => (
            <Field key={key} orientation="horizontal">
              <Checkbox
                id={`raw-sealed-${key}`}
                checked={sealing.includes(key)}
                onCheckedChange={(checked) => {
                  warmSealing();
                  onSealingChange(key, Boolean(checked));
                }}
              />
              <FieldLabel htmlFor={`raw-sealed-${key}`}>
                Seal <span className="font-mono text-xs">{key}</span>
              </FieldLabel>
              {problems.has(key) ? <FieldError>{problems.get(key)}</FieldError> : null}
            </Field>
          ))}
        </FieldGroup>
      ) : null}
    </div>
  );
}

function ServiceVariablesRawEditorFooter({
  text,
  mode,
  onCancel,
  onSubmit,
}: {
  text: string;
  mode: RawEditorMode;
  onCancel: () => void;
  onSubmit: () => void;
}) {
  return (
    <DialogFooter className="min-w-0 sm:flex-wrap sm:justify-between">
      <CopyButton value={text} label={`Copy ${mode.toUpperCase()}`} showLabel variant="outline" />
      <div className="flex min-w-0 flex-wrap items-center justify-end gap-2">
        <Button
          type="button"
          variant="outline"
          onClick={onCancel}
        >
          Cancel
        </Button>
        <Button
          type="button"
          onClick={onSubmit}
        >
          Update variables
        </Button>
      </div>
    </DialogFooter>
  );
}

export function ServiceVariablesRawEditor({
  open,
  onOpenChange,
  onApply,
  variables,
  valueTargets = EMPTY_REFERENCE_TARGETS,
  sealing,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Saves the editor's creates, updates and deletes: optimistic, rolled back and toasted on failure. */
  onApply: (diff: RawEditorDiff) => Write;
  variables: VariableRecord[];
  valueTargets?: ReferenceTarget[];
  /** `useSealing`'s cached state: ready offers Sealed for new keys. Display only. */
  sealing?: "no_server" | "ready";
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
  const refusedRef = useRef<Pick<RawEditorState, "mode" | "envText" | "jsonText" | "submitError" | "sealing"> | null>(null);

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

    if (entries.some((entry) => editor.sealing.includes(entry.key) && sealedValueProblem(entry.key, entry.value) !== null)) return;
    // A ticked line goes Sealed whatever the cached state says, and whether another tab made its key meanwhile: the
    // write's fresh read seals it or refuses.
    const diff = diffVariables(entries, editableVariables, new Set(editor.sealing));
    if (
      diff.creates.length === 0 &&
      diff.updates.length === 0 &&
      diff.deletes.length === 0
    ) {
      onOpenChange(false);
      return;
    }

    const typed = { mode: editor.mode, envText: editor.envText, jsonText: editor.jsonText, sealing: editor.sealing };
    onApply(diff).catch((error) => {
      refusedRef.current = { ...typed, submitError: toErrorMessage(error, "The variables couldn’t be saved.") };
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

  const ready = sealing === "ready";
  // Ticks outlive a cached state that leaves ready, so they stay in view until untick or submit.
  const offered = ready || editor.sealing.length > 0;
  const parsed = offered ? parseForMode(editor.mode === "env" ? editor.envText : editor.jsonText, editor.mode) : null;
  const taken = new Set(variables.map((variable) => variable.key));
  const newKeys = parsed && Result.isSuccess(parsed)
    ? [...new Set(parsed.success.flatMap((entry) => taken.has(entry.key) && !editor.sealing.includes(entry.key) ? [] : [entry.key]))] : [];
  const problems = new Map(parsed && Result.isSuccess(parsed) ? parsed.success.flatMap((entry) => {
    const problem = editor.sealing.includes(entry.key) ? sealedValueProblem(entry.key, entry.value) : null;
    return problem === null ? [] : [[entry.key, problem] as const];
  }) : []);

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
          sealedCount={offered ? 0 : sealedCount}
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

        {offered ? (
          <ServiceVariablesRawEditorSealed
            sealedKeys={sealedVariables.map((variable) => variable.key)}
            newKeys={newKeys}
            sealing={editor.sealing}
            problems={problems}
            onSealingChange={(key, sealed) => dispatchEditor({
              type: "patch",
              patch: { sealing: sealed ? [...editor.sealing, key] : editor.sealing.filter((one) => one !== key) },
            })}
          />
        ) : null}

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
