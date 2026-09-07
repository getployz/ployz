import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { eq } from "drizzle-orm";
import { Result } from "effect";
import * as schema from "#/db/schema";
import {
  type GithubPostgresTestHarness,
  startGithubPostgresTestHarness,
} from "#/modules/github/github-ingestion.postgres-test-harness";
import { restoreServiceWorkingIntentWithExecutor } from "#/modules/services/service-working-intent.server";
import { savedEnvironmentIntentSchema } from "#/modules/environment-design/saved-intent";
import { decodeStrict } from "#/modules/environment-design/schema";

const organizationId = "00000000-0000-4000-8000-000000000301";
const userId = "00000000-0000-4000-8000-000000000302";
const projectId = "00000000-0000-4000-8000-000000000303";
const environmentId = "00000000-0000-4000-8000-000000000304";
const serviceLineageId = "00000000-0000-4000-8000-000000000305";
const serviceId = "00000000-0000-4000-8000-000000000306";
const savedStateSnapshotId = "00000000-0000-4000-8000-000000000307";
const savedVolumeId = "00000000-0000-4000-8000-000000000308";
const workingVolumeId = "00000000-0000-4000-8000-000000000309";
const savedVolumeLineageId = "00000000-0000-4000-8000-000000000310";
const workingVolumeLineageId = "00000000-0000-4000-8000-000000000311";
const workingVariableGroupLineageId = "00000000-0000-4000-8000-000000000312";
const workingVariableGroupId = "00000000-0000-4000-8000-000000000313";
const workingConfigKeyId = "00000000-0000-4000-8000-000000000314";
const workingVariableId = "00000000-0000-4000-8000-000000000315";
const encryptedCredential = {
  version: 1 as const,
  iv: "working-iv",
  tag: "working-tag",
  ciphertext: "working-ciphertext",
};

describe("complete Service Working Intent reset", () => {
  let harness: GithubPostgresTestHarness;

  beforeAll(async () => {
    harness = await startGithubPostgresTestHarness();
    await harness.pool.query(`
      truncate table environment_saved_state_snapshot, service_volume_attachment,
        service_variable_group_attachment, variable, config_key,
        environment_resource, resource_lineage, environment_variable_group,
        variable_group_lineage, service, service_lineage, environment, project,
        "user", organization cascade;
      insert into organization (id, name, slug)
      values ('${organizationId}', 'Acceptance', 'acceptance');
      insert into "user" (id, email, name)
      values ('${userId}', 'owner@example.com', 'Owner');
      insert into project (id, organization_id, name, slug)
      values ('${projectId}', '${organizationId}', 'Cloud', 'cloud');
      insert into environment (
        id, project_id, organization_id, name, namespace
      ) values (
        '${environmentId}', '${projectId}', '${organizationId}',
        'Production', 'production'
      );
      insert into service_lineage (
        id, project_id, canonical_name, canonical_slug
      ) values ('${serviceLineageId}', '${projectId}', 'API', 'api');
      insert into service (
        id, project_id, environment_id, organization_id, lineage_id, name, slug,
        source_type, source_config, private_dns, replicas,
        has_registry_credential
      ) values (
        '${serviceId}', '${projectId}', '${environmentId}', '${organizationId}',
        '${serviceLineageId}', 'Working API', 'api', 'empty',
        '{"version":1,"type":"empty","rootDir":"/"}', 'working-api', 3,
        true
      );
      insert into service_registry_credential (
        service_id, encrypted_registry_secret
      ) values ('${serviceId}', '${JSON.stringify(encryptedCredential)}');
      insert into variable_group_lineage (
        id, project_id, canonical_name, canonical_slug
      ) values (
        '${workingVariableGroupLineageId}', '${projectId}',
        'Working group', 'working-group'
      );
      insert into environment_variable_group (
        id, organization_id, project_id, environment_id, lineage_id, name, slug
      ) values (
        '${workingVariableGroupId}', '${organizationId}', '${projectId}',
        '${environmentId}', '${workingVariableGroupLineageId}',
        'Working group', 'working-group'
      );
      insert into service_variable_group_attachment (
        organization_id, service_id, variable_group_id, sort_order
      ) values (
        '${organizationId}', '${serviceId}', '${workingVariableGroupId}', 0
      );
      insert into config_key (
        id, project_id, scope, service_lineage_id, canonical_name
      ) values (
        '${workingConfigKeyId}', '${projectId}', 'service_lineage',
        '${serviceLineageId}', 'WORKING_ONLY'
      );
      insert into variable (
        id, organization_id, project_id, service_id, config_key_id, key,
        value_kind, value_parts, value_fingerprint
      ) values (
        '${workingVariableId}', '${organizationId}', '${projectId}',
        '${serviceId}', '${workingConfigKeyId}', 'WORKING_ONLY', 'plain',
        '[{"kind":"text","value":"working"}]', 'working-fingerprint'
      );
      insert into resource_lineage (
        id, organization_id, project_id, canonical_name, canonical_slug
      ) values
        ('${savedVolumeLineageId}', '${organizationId}', '${projectId}', 'Saved data', 'saved-data'),
        ('${workingVolumeLineageId}', '${organizationId}', '${projectId}', 'Working data', 'working-data');
      insert into environment_resource (
        id, organization_id, project_id, environment_id, lineage_id,
        implementation_type, name, slug
      ) values
        ('${savedVolumeId}', '${organizationId}', '${projectId}', '${environmentId}',
         '${savedVolumeLineageId}', 'volume', 'Saved data', 'saved-data'),
        ('${workingVolumeId}', '${organizationId}', '${projectId}', '${environmentId}',
         '${workingVolumeLineageId}', 'volume', 'Working data', 'working-data');
      insert into service_volume_attachment (
        organization_id, project_id, environment_id, service_id,
        volume_resource_id, mount_path
      ) values (
        '${organizationId}', '${projectId}', '${environmentId}', '${serviceId}',
        '${workingVolumeId}', '/working'
      );
    `);

    const intent = decodeStrict(savedEnvironmentIntentSchema, {
      version: 1,
      environmentSlug: "production",
      services: [
        {
          id: serviceId,
          lineageId: serviceLineageId,
          slug: "api",
          config: {
            version: 2,
            name: "Saved API",
            source: { version: 1, type: "empty", rootDir: "/" },
            preDeployCommand: null,
            startCommand: null,
            healthcheck: { type: "none" },
            restartPolicy: "unless-stopped",
            maxRetries: 10,
            cron: null,
            replicas: 1,
            cpuLimit: null,
            memLimit: null,
            privateDns: "saved-api",
            routes: [],
            managedHostname: null,
            build: {
              builder: "auto",
              dockerfilePath: null,
              watchPaths: [],
            },
          },
          variables: [],
          variableGroupAttachments: [],
          volumeAttachments: [
            { volumeResourceId: savedVolumeId, mountPath: "/data" },
          ],
          encryptedRegistryUsername: null,
          encryptedRegistrySecret: null,
        },
      ],
      variableGroups: [],
      volumes: [
        {
          resourceId: savedVolumeId,
          resourceLineageId: savedVolumeLineageId,
          name: "Saved data",
        },
      ],
    });
    await harness.pool.query(
      `insert into environment_saved_state_snapshot (
        id, organization_id, environment_id, actor_id, intent,
        volume_deletion_authorizations
      ) values ($1, $2, $3, $4, $5, '[]')`,
      [
        savedStateSnapshotId,
        organizationId,
        environmentId,
        userId,
        JSON.stringify(intent),
      ],
    );
  }, 60_000);

  afterAll(async () => {
    await harness?.stop();
  });

  it("replaces the Service row and its owned volume edges from one Saved revision", async () => {
    const restored = await harness.runTransactionResult((tx) =>
      restoreServiceWorkingIntentWithExecutor(
        {
          organizationId,
          projectId,
          environmentId,
          serviceId,
          savedStateSnapshotId,
        },
        tx,
      ),
    );
    expect(Result.isSuccess(restored)).toBe(true);

    const [service] = await harness.db
      .select({
        name: schema.service.name,
        privateDns: schema.service.privateDns,
        replicas: schema.service.replicas,
        hasRegistryCredential: schema.service.hasRegistryCredential,
      })
      .from(schema.service)
      .where(eq(schema.service.id, serviceId));
    const attachments = await harness.db
      .select({
        volumeResourceId: schema.serviceVolumeAttachment.volumeResourceId,
        mountPath: schema.serviceVolumeAttachment.mountPath,
      })
      .from(schema.serviceVolumeAttachment)
      .where(eq(schema.serviceVolumeAttachment.serviceId, serviceId));
    const [variables, variableGroupAttachments, registryCredentials] =
      await Promise.all([
        harness.db
          .select({ id: schema.variable.id })
          .from(schema.variable)
          .where(eq(schema.variable.serviceId, serviceId)),
        harness.db
          .select({
            variableGroupId:
              schema.serviceVariableGroupAttachment.variableGroupId,
          })
          .from(schema.serviceVariableGroupAttachment)
          .where(eq(schema.serviceVariableGroupAttachment.serviceId, serviceId)),
        harness.db
          .select({ serviceId: schema.serviceRegistryCredential.serviceId })
          .from(schema.serviceRegistryCredential)
          .where(eq(schema.serviceRegistryCredential.serviceId, serviceId)),
      ]);

    expect(service).toEqual({
      name: "Saved API",
      privateDns: "saved-api",
      replicas: 1,
      hasRegistryCredential: false,
    });
    expect(attachments).toEqual([
      { volumeResourceId: savedVolumeId, mountPath: "/data" },
    ]);
    expect(variables).toEqual([]);
    expect(variableGroupAttachments).toEqual([]);
    expect(registryCredentials).toEqual([]);
  });
});
