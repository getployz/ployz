/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "Namespace".
 */
export type Namespace = string;
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "NetworkMode".
 */
export type NetworkMode =
  | ("overlay" | "host" | "none")
  | {
      service: string;
    };
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "Placement".
 */
export type Placement =
  | "global"
  | {
      replicated: {
        count: number;
        [k: string]: any;
      };
    };
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "ReadinessProbe".
 */
export type ReadinessProbe = {
  interval?: string | null;
  retries?: number | null;
  start_period?: string | null;
  timeout?: string | null;
  [k: string]: any;
} & ReadinessProbe1;
export type ReadinessProbe1 =
  | {
      http: {
        path: string;
        service_port: string;
        [k: string]: any;
      };
    }
  | {
      tcp: {
        service_port: string;
        [k: string]: any;
      };
    }
  | {
      exec: {
        command: string[];
        [k: string]: any;
      };
    };
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "RestartPolicy".
 */
export type RestartPolicy = "unless-stopped" | "always" | "on-failure" | "no";
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "RolloutStrategy".
 */
export type RolloutStrategy = "recreate" | "blue_green";
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "RouteSpec".
 */
export type RouteSpec =
  | {
      http: HttpRoute;
    }
  | {
      tcp: TcpRoute;
    };
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "PortProtocol".
 */
export type PortProtocol = "tcp" | "udp";
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "MountSource".
 */
export type MountSource =
  | "tmpfs"
  | {
      volume: string;
    }
  | {
      bind: string;
    };
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "PullPolicy".
 */
export type PullPolicy = "if_not_present" | "always" | "never";
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "VolumeScope".
 */
export type VolumeScope = "single" | "shared";

export interface DeployManifest {
  namespace: Namespace;
  services: ServiceSpec[];
  volumes?: VolumeDeclaration[];
  [k: string]: any;
}
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "ServiceSpec".
 */
export interface ServiceSpec {
  labels?: {
    [k: string]: string;
  };
  name: string;
  network: NetworkMode;
  placement: Placement;
  publish?: PublishedPort[];
  readiness?: ReadinessProbe | null;
  restart?: RestartPolicy & string;
  rollout?: RolloutStrategy & string;
  routes?: RouteSpec[];
  service_ports?: ServicePort[];
  template: ContainerSpec;
  [k: string]: any;
}
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "PublishedPort".
 */
export interface PublishedPort {
  host_ip?: string | null;
  host_port: number;
  service_port: string;
  [k: string]: any;
}
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "HttpRoute".
 */
export interface HttpRoute {
  hostnames?: string[];
  path_prefix?: string;
  service_port: string;
  [k: string]: any;
}
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "TcpRoute".
 */
export interface TcpRoute {
  listen_port: number;
  service_port: string;
  [k: string]: any;
}
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "ServicePort".
 */
export interface ServicePort {
  container_port: number;
  name: string;
  protocol?: PortProtocol & string;
  [k: string]: any;
}
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "ContainerSpec".
 */
export interface ContainerSpec {
  cap_add?: string[];
  cap_drop?: string[];
  command?: string[] | null;
  entrypoint?: string[] | null;
  env?: {
    [k: string]: string;
  };
  image: string;
  mounts?: Mount[];
  pid_mode?: string | null;
  privileged?: boolean;
  pull_policy?: PullPolicy & string;
  resources?: Resources;
  stop_grace_period?: string | null;
  sysctls?: {
    [k: string]: string;
  };
  user?: string | null;
  [k: string]: any;
}
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "Mount".
 */
export interface Mount {
  readonly?: boolean;
  source: MountSource;
  target: string;
  [k: string]: any;
}
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "Resources".
 */
export interface Resources {
  cpu_millicores?: number | null;
  memory_bytes?: number | null;
  [k: string]: any;
}
/**
 * This interface was referenced by `DeployManifest`'s JSON-Schema
 * via the `definition` "VolumeDeclaration".
 */
export interface VolumeDeclaration {
  mode: string;
  name: string;
  owner: string;
  quota: string;
  scope: VolumeScope;
  [k: string]: any;
}
