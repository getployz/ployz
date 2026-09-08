import "@tanstack/react-start/server-only";
import { randomUUID } from "node:crypto";
import { Effect } from "effect";
import type { Actor } from "#/modules/identity/actor";
import { withMutationReceipt } from "#/server/mutation-receipt.server";
import { Conflict, NotFound, Validation } from "#/server/public-error";
import { SecretEncryption } from "#/utils/encrypted-secret.server";
import { requireEnvironmentForActorById } from "./authoring-repository.server";
import { loadEnvironmentDocument, requireDocumentRevision, writeEnvironmentDocument } from "./working-state-repository.server";
import { environmentVariableReferences } from "./variable-document";
import { validateVariableValue, variableValueColumnsForWrite } from "./variable-repository.server";
import { savedVariableIntent, type SavedVariableIntent } from "./saved-intent";
import type { BulkUpdateServiceVariablesInput, CreateServiceVariableInput, CreateVariableGroupVariableInput, UpdateServiceVariableExportInput, UpdateServiceVariableInput, UpdateVariableGroupVariableInput, UpdateVariableGroupVariableMetadataInput, VariableValueInput } from "./variables";

type Scope = { readonly organizationSlug: string; readonly environmentId: string; readonly revision: string };
type ServiceScope = Scope & { readonly serviceId: string };
type GroupScope = Scope & { readonly variableGroupId: string };
type VariableEdit = { id?: string; variableId?: string; key?: string; description?: string | null; exported?: boolean; value?: VariableValueInput };

const editVariables = Effect.fn("EnvironmentDesign.editVariables")(
  function* (actor: Actor, input: ServiceScope | GroupScope, edits: readonly VariableEdit[], deletes: readonly string[] = []) {
    yield* requireEnvironmentForActorById(actor, input);
    const encryption = yield* SecretEncryption;
    return yield* withMutationReceipt(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      const node = "serviceId" in input ? document.intent.services.find((node) => node.id === input.serviceId)
        : document.intent.variableGroups.find((node) => node.variableGroupId === input.variableGroupId);
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
          const invalid = validateVariableValue(edit.value, { lookupLineage: refs.lookupLineage, ownerScope: "serviceId" in input ? "service" : "variable_group" });
          if (invalid) return yield* invalid;
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
export const createVariableGroupVariable = Effect.fn("EnvironmentDesign.createVariableGroupVariable")(
  (actor: Actor, input: CreateVariableGroupVariableInput) => editVariables(actor, input, [input]),
);
export const updateServiceVariable = Effect.fn("EnvironmentDesign.updateServiceVariable")(
  (actor: Actor, input: UpdateServiceVariableInput) => editVariables(actor, input, [input]),
);
export const updateVariableGroupVariable = Effect.fn("EnvironmentDesign.updateVariableGroupVariable")(
  (actor: Actor, input: UpdateVariableGroupVariableInput) => editVariables(actor, input, [input]),
);
export const updateServiceVariableExport = Effect.fn("EnvironmentDesign.updateServiceVariableExport")(
  (actor: Actor, input: UpdateServiceVariableExportInput) => editVariables(actor, input, [input]),
);
export const updateVariableGroupVariableMetadata = Effect.fn("EnvironmentDesign.updateVariableGroupVariableMetadata")(
  (actor: Actor, input: UpdateVariableGroupVariableMetadataInput) => editVariables(actor, input, [input]),
);
export const deleteServiceVariable = Effect.fn("EnvironmentDesign.deleteServiceVariable")(
  (actor: Actor, input: ServiceScope & { readonly variableId: string }) => editVariables(actor, input, [], [input.variableId]),
);
export const deleteVariableGroupVariable = Effect.fn("EnvironmentDesign.deleteVariableGroupVariable")(
  (actor: Actor, input: GroupScope & { readonly variableId: string }) => editVariables(actor, input, [], [input.variableId]),
);
export const bulkUpdateServiceVariables = Effect.fn("EnvironmentDesign.bulkUpdateServiceVariables")(
  (actor: Actor, input: BulkUpdateServiceVariablesInput) => editVariables(actor, input, [...input.updates, ...input.creates], input.deletes),
);

const editGroupAttachment = Effect.fn("EnvironmentDesign.editGroupAttachment")(
  function* (actor: Actor, input: ServiceScope & { readonly variableGroupId: string }, attach: boolean) {
    yield* requireEnvironmentForActorById(actor, input);
    return yield* withMutationReceipt(Effect.gen(function* () {
      const document = yield* loadEnvironmentDocument(input.environmentId, true);
      yield* requireDocumentRevision(document, input.revision);
      const node = document.intent.services.find((node) => node.id === input.serviceId);
      if (!node) return yield* new NotFound({ message: "Service not found." });
      if (!document.intent.variableGroups.some((group) => group.variableGroupId === input.variableGroupId)) return yield* new NotFound({ message: "Variable group not found." });
      if (attach) {
        if (!node.variableGroupAttachments.some((attachment) => attachment.variableGroupId === input.variableGroupId)) node.variableGroupAttachments.push({ variableGroupId: input.variableGroupId, sortOrder: Math.max(-1, ...node.variableGroupAttachments.map((attachment) => attachment.sortOrder)) + 1 });
      } else node.variableGroupAttachments = node.variableGroupAttachments.filter((attachment) => attachment.variableGroupId !== input.variableGroupId);
      return yield* writeEnvironmentDocument(document, document.intent);
    }));
  },
);
export const attachServiceVariableGroup = Effect.fn("EnvironmentDesign.attachServiceVariableGroup")(
  (actor: Actor, input: ServiceScope & { readonly variableGroupId: string }) => editGroupAttachment(actor, input, true),
);
export const detachServiceVariableGroup = Effect.fn("EnvironmentDesign.detachServiceVariableGroup")(
  (actor: Actor, input: ServiceScope & { readonly variableGroupId: string }) => editGroupAttachment(actor, input, false),
);
