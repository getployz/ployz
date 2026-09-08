import { createCollection, localOnlyCollectionOptions } from "@tanstack/react-db";
import { parseServiceConfig } from "@ployz/sdk/config";
import { expect, it, vi } from "vitest";
import type { EnvironmentDocument } from "#/modules/environment-design/working-state-repository.server";
import { buildCanvasEnvironmentChangeState } from "#/modules/environment-design/canvas-environment-change-state";
import { createWorkingSettingRestoreAction } from "./working-setting-restore-action";

const id = (n: number) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;

it.each(["variableGroupAttachments", "source.credentials", "source"])(
  "restores %s through authored history and rolls back stale optimistic writes",
  async (path) => {
    const restore = vi.fn<(input: { data: import("./working-document-restore").RestoreWorkingDocumentInput }) => Promise<{ txid: number }>>();
    const baseline = parseServiceConfig({ version: 2, name: "API", privateDns: "api",
      source: path === "source" ? { version: 1, type: "empty", rootDir: "/" }
        : { version: 1, type: "image", image: "nginx", autoUpdate: { type: "off" }, credentials: { type: "configured", revision: "before" } },
      preDeployCommand: null, startCommand: null, healthcheck: { type: "none" }, restartPolicy: "unless-stopped",
    });
    const current = parseServiceConfig({ ...baseline, replicas: 3,
      source: { version: 1, type: "image", image: "nginx", autoUpdate: { type: "off" }, credentials: { type: "configured", revision: "after" } },
      variableGroupAttachments: [{ variableGroupId: id(7), sortOrder: 0 }],
    });
    const { env: _env, mounts: _mounts, variableGroupAttachments, ...config } = current;
    const original: EnvironmentDocument = { id: id(1), organizationId: id(2), projectId: id(3), name: "Production",
      namespace: "production", revision: id(4), createdAt: new Date(), updatedAt: new Date(),
      intent: { version: 1, environmentSlug: "production", services: [{ id: id(5), lineageId: id(6), slug: "api",
        config, variables: [], variableGroupAttachments, volumeAttachments: [], encryptedRegistryUsername: null, encryptedRegistrySecret: null }],
        variableGroups: [], volumes: [] },
    };
    const environments = createCollection(localOnlyCollectionOptions({ getKey: (row: EnvironmentDocument) => row.id, initialData: [original] }));

    const node = { type: "service" as const, id: id(5) };
    const before = { token: "before", nodes: [{ node, config: baseline }] };
    const intro = path === "variableGroupAttachments";
    const changeState = buildCanvasEnvironmentChangeState({
      working: { token: "working", nodes: [{ node, config: current }] },
      saved: intro ? { kind: "no_saved_state", token: "none", nodes: [] }
        : { ...before, kind: "saved_revision", savedStateSnapshotId: id(8) },
      applied: intro ? { token: "none", nodes: [] } : before,
      nodeIntroductions: intro ? before : { token: "none", nodes: [] }, runtimeObserved: null,
      deploymentEvidence: null, nodes: [{ node, name: "API", summaryLabel: "API" }],
    });
    const group = changeState.slices.unsaved.groups[0];
    expect(group?.rows.find((row) => row.path === path)?.canDiscard).toBe(true);
    if (!group) throw new Error("Expected an unsaved Service change.");
    let reject: (error: Error) => void = () => { throw new Error("Restore has not started."); };
    restore.mockImplementation(() => new Promise((_resolve, fail) => { reject = fail; }));
    const action = createWorkingSettingRestoreAction({ environments, environmentId: id(1), organizationSlug: "acme",
      restore, awaitTxId: async () => {},
    });
    const setting = group.projectedChange.settings.find((setting) => setting.owner.setting === path);
    if (!setting?.discardPlan) throw new Error("Expected a discard plan.");
    const failed = action({ serviceId: id(5), revision: id(4), path,
      baseline: parseServiceConfig(setting.discardPlan.config),
      snapshotSource: setting.baselineSource?.role === "node_introduction" ? { kind: "introduction" }
        : { kind: "saved", environmentSavedStateSnapshotId: id(8) },
    }).isPersisted.promise.catch((error: Error) => error);
    await Promise.race([failed.then((error) => { if (error) throw error; }), vi.waitFor(() => expect(restore).toHaveBeenCalledOnce())]);
    const optimistic = environments.get(id(1))?.intent.services[0];
    expect(path === "variableGroupAttachments" ? optimistic?.variableGroupAttachments : optimistic?.config.source)
      .toEqual(path === "variableGroupAttachments" ? baseline.variableGroupAttachments : baseline.source);
    expect(optimistic?.config.replicas).toBe(3);
    expect(restore).toHaveBeenCalledWith({ data: { organizationSlug: "acme", environmentId: id(1), revision: id(4),
      snapshotSource: intro ? { kind: "introduction" } : { kind: "saved", environmentSavedStateSnapshotId: id(8) },
      command: { kind: "node", nodeType: "service", nodeId: id(5), path } } });
    expect(JSON.stringify(restore.mock.calls)).not.toContain("ciphertext");
    reject(new Error("Working State changed"));
    expect(await failed).toMatchObject({ message: "Working State changed" });
    expect(environments.get(id(1))?.intent).toEqual(original.intent);
    expect(environments.get(id(1))?.revision).toBe(original.revision);
    await environments.cleanup();
  },
);
