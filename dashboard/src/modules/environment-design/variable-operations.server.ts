import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { withMutationResult } from "#/server/mutation-result.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { requireEnvironmentForActorById } from "./authoring-repository.server";
import { loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "./working-state-repository.server";
import { environmentVariableReferences } from "./variable-document";
import { variableValueColumnsForWrite } from "./variable-repository.server";
import { savedVariableIntent, type SavedVariableIntent } from "./saved-intent";
import type { BulkUpdateServiceVariablesInput, CreateServiceVariableInput, UpdateServiceVariableExportInput, UpdateServiceVariableInput, VariableValueInput } from "./variables";

type Scope = { readonly organizationSlug: string; readonly environmentId: string; readonly revision: string };
type ServiceScope = Scope & { readonly serviceId: string };
type VariableEdit = { id?: string; variableId?: string; key?: string; description?: string | null; exported?: boolean; value?: VariableValueInput };

const editVariables = Effect.fn("EnvironmentDesign.editVariables")(
  function* (actor: Actor, input: ServiceScope, edits: readonly VariableEdit[], deletes: readonly string[] = []) {
    yield* requireEnvironmentForActorById(actor, input);
    const encryption = yield* SecretEncryption;
    return yield* withMutationResult(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      const node = document.intent.services.find((node) => node.id === input.serviceId);
      if (!node) return yield* new NotFound({ message: "Variable owner not found." });
      for (const id of deletes) if (!node.variables.some((variable) => variable.id === id)) return yield* new NotFound({ message: "Variable not found." });
      node.variables = node.variables.filter((variable) => !deletes.includes(variable.id));
      const refs = environmentVariableReferences(document.intent);
      for (const edit of edits) {
        const existing = edit.variableId ? node.variables.find((variable) => variable.id === edit.variableId) : undefined;
        if (edit.variableId && !existing) return yield* new NotFound({ message: "Variable not found." });
        if (existing?.value.kind === "secret" && edit.value?.type === "plain") return yield* new Validation({ message: "Sealed variables cannot be converted back to plain variables." });
        let next: SavedVariableIntent;
        if (edit.value) {
          next = savedVariableIntent({ id: existing?.id ?? edit.id ?? randomUUID(), key: edit.key ?? existing?.key ?? "",
            description: edit.description === undefined ? existing?.description ?? null : edit.description,
            exported: edit.exported ?? existing?.exported ?? false,
            ...variableValueColumnsForWrite(encryption, edit.value, refs.lookupLineage) });
        } else {
          if (!existing) return yield* new Validation({ message: "A variable value is required." });
          next = { ...existing, description: edit.description === undefined ? existing.description : edit.description, exported: edit.exported ?? existing.exported };
        }
        if (existing) node.variables[node.variables.indexOf(existing)] = next;
        else node.variables.push(next);
      }
      const keys = new Set<string>();
      for (const variable of node.variables) {
        if (keys.has(variable.key)) return yield* new Conflict({ message: `Duplicate variable key: ${variable.key}` });
        keys.add(variable.key);
      }
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
  },
);

export const createServiceVariable = Effect.fn("EnvironmentDesign.createServiceVariable")(
  (actor: Actor, input: CreateServiceVariableInput) => editVariables(actor, input, [input]),
);
export const updateServiceVariable = Effect.fn("EnvironmentDesign.updateServiceVariable")(
  (actor: Actor, input: UpdateServiceVariableInput) => editVariables(actor, input, [input]),
);
export const updateServiceVariableExport = Effect.fn("EnvironmentDesign.updateServiceVariableExport")(
  (actor: Actor, input: UpdateServiceVariableExportInput) => editVariables(actor, input, [input]),
);
export const deleteServiceVariable = Effect.fn("EnvironmentDesign.deleteServiceVariable")(
  (actor: Actor, input: ServiceScope & { readonly variableId: string }) => editVariables(actor, input, [], [input.variableId]),
);
export const bulkUpdateServiceVariables = Effect.fn("EnvironmentDesign.bulkUpdateServiceVariables")(
  (actor: Actor, input: BulkUpdateServiceVariablesInput) => editVariables(actor, input, [...input.updates, ...input.creates], input.deletes),
);
