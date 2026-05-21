export type RuntimeWatchFrame =
  | {
      kind: "snapshot";
      state: RoutingState;
      [k: string]: any;
    }
  | {
      key: string;
      kind: "machine_upsert";
      record: MachineMembership;
      [k: string]: any;
    }
  | {
      id: MachineId;
      key: string;
      kind: "machine_remove";
      [k: string]: any;
    }
  | {
      key: string;
      kind: "revision_upsert";
      record: ServiceRevisionRecord;
      [k: string]: any;
    }
  | {
      key: string;
      kind: "revision_remove";
      namespace: Namespace;
      revision_hash: string;
      service: string;
      [k: string]: any;
    }
  | {
      key: string;
      kind: "release_upsert";
      record: ServiceReleaseRecord;
      [k: string]: any;
    }
  | {
      key: string;
      kind: "release_remove";
      namespace: Namespace;
      service: string;
      [k: string]: any;
    }
  | {
      key: string;
      kind: "instance_upsert";
      record: InstanceStatusRecord;
      [k: string]: any;
    }
  | {
      instance_id: InstanceId;
      key: string;
      kind: "instance_remove";
      [k: string]: any;
    }
  | {
      code: string;
      kind: "error";
      message: string;
      [k: string]: any;
    }
  | {
      kind: "heartbeat";
      [k: string]: any;
    };
export type DeployId = string;
export type DrainState = "None" | "Requested" | "Complete";
export type InstanceId = string;
export type MachineId = string;
export type Namespace = string;
export type InstancePhase = "Pending" | "Starting" | "Ready" | "Failed" | "Draining" | "Removed";
export type SlotId = string;
export type OverlayIp = string;
export type MachineLifecycle = "Standby" | "Active" | "Draining";
/**
 * @minItems 32
 * @maxItems 32
 */
export type PublicKey = [
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number,
  number
];
export type RegionRole = "home_data" | "compute" | "disabled" | "draining";
export type MachineStorageRole =
  | {
      kind: "compute";
      [k: string]: any;
    }
  | {
      kind: "candidate";
      [k: string]: any;
    }
  | {
      authority_id: AuthorityId;
      kind: "authority";
      [k: string]: any;
    };
export type AuthorityId = string;
export type AvailabilityZoneName = string;
export type RegionName = string;
export type ServiceReleaseTarget =
  | {
      kind: "direct";
      revision_hash: string;
      [k: string]: any;
    }
  | {
      allocations: ServiceTrafficAllocation[];
      kind: "split";
      primary_revision_hash: string;
      [k: string]: any;
    };

export interface RoutingState {
  instances: InstanceStatusRecord[];
  machines: MachineMembership[];
  releases: ServiceReleaseRecord[];
  revisions: ServiceRevisionRecord[];
  [k: string]: any;
}
export interface InstanceStatusRecord {
  backend_ports: {
    [k: string]: number;
  };
  deploy_id: DeployId;
  docker_container_id: string;
  drain_state: DrainState;
  error?: string | null;
  instance_id: InstanceId;
  machine_id: MachineId;
  namespace: Namespace;
  overlay_ip?: string | null;
  phase: InstancePhase;
  ready: boolean;
  revision_hash: string;
  service: string;
  slot_id: SlotId;
  started_at: number;
  updated_at: number;
  [k: string]: any;
}
export interface MachineMembership {
  bridge_ip?: OverlayIp | null;
  created_at: number;
  endpoints: string[];
  id: MachineId;
  labels: {
    [k: string]: string;
  };
  lifecycle?: MachineLifecycle & string;
  overlay_ip: OverlayIp;
  public_key: PublicKey;
  region_role: RegionRole;
  storage_role: MachineStorageRole;
  subnet?: string | null;
  topology: MachineTopology;
  updated_at: number;
  [k: string]: any;
}
export interface MachineTopology {
  availability_zone?: AvailabilityZoneName | null;
  region: RegionName;
  [k: string]: any;
}
export interface ServiceReleaseRecord {
  namespace: Namespace;
  release: ServiceRelease;
  service: string;
  [k: string]: any;
}
export interface ServiceRelease {
  slots: ServiceReleaseSlot[];
  target: ServiceReleaseTarget;
  updated_at: number;
  updated_by_deploy_id: DeployId;
  [k: string]: any;
}
export interface ServiceReleaseSlot {
  active_instance_id: InstanceId;
  machine_id: MachineId;
  revision_hash: string;
  slot_id: SlotId;
  [k: string]: any;
}
export interface ServiceTrafficAllocation {
  label?: string | null;
  percent: number;
  revision_hash: string;
  [k: string]: any;
}
export interface ServiceRevisionRecord {
  created_at: number;
  created_by: MachineId;
  namespace: Namespace;
  revision_hash: string;
  service: string;
  spec_json: string;
  [k: string]: any;
}
