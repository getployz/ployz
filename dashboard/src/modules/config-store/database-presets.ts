import type { ConfigCommand, EnvironmentRef } from "@ployz/sdk";
import { DEFAULT_VOLUME_GB } from "./store-volumes";

/**
 * A Database Preset: a common database set up as Railway sets it up (image, data path, start command, variables),
 * created as an ordinary image Service with a Volume and a healthcheck that passes once the database answers.
 * Variables reference each other as Railway's do, with `PLOYZ_PRIVATE_DOMAIN` for `RAILWAY_PRIVATE_DOMAIN`, and all
 * are exported so other Services can reference them.
 */
// ponytail: the password is only read when the database first initializes its Volume; changing it later needs the
// database's own tooling too. No rotation: the Store keeps a generated value until it is set by hand.
export type DatabasePreset = {
  id: "postgres" | "redis" | "mongodb" | "mysql";
  label: string;
  image: string;
  dataPath: string;
  startCommand?: string;
  /** A command run in the container, where the preset's variables are set. */
  healthcheck: string;
  env: Record<string, string>;
};

const HOST = "${{ PLOYZ_PRIVATE_DOMAIN }}";

/**
 * 32 random letters, as Railway's `secret(32, "a-zA-Z")`. The Store makes the value, plain, and a Branch copy or PR
 * Environment makes its own, so each New, empty Volume initializes with a password of its own.
 */
export const GENERATED_PASSWORD = '${{ secret(32, "a-zA-Z") }}';

export const DATABASE_PRESETS: readonly DatabasePreset[] = [
  {
    id: "postgres", label: "PostgreSQL",
    image: "ghcr.io/railwayapp-templates/postgres-ssl:18", dataPath: "/var/lib/postgresql/data",
    healthcheck: "pg_isready -h 127.0.0.1 -p 5432",
    env: {
      POSTGRES_USER: "postgres", POSTGRES_PASSWORD: GENERATED_PASSWORD, POSTGRES_DB: "ployz",
      PGDATA: "/var/lib/postgresql/data/pgdata", SSL_CERT_DAYS: "820",
      PGUSER: "${{ POSTGRES_USER }}", PGPASSWORD: "${{ POSTGRES_PASSWORD }}", PGDATABASE: "${{ POSTGRES_DB }}",
      PGHOST: HOST, PGPORT: "5432",
      DATABASE_URL: `postgresql://\${{ PGUSER }}:\${{ POSTGRES_PASSWORD }}@${HOST}:5432/\${{ PGDATABASE }}`,
    },
  },
  {
    id: "redis", label: "Redis", image: "redis:8.2", dataPath: "/data",
    // An image's start command runs without a shell, so the password needs one to expand.
    startCommand: "sh -c 'exec docker-entrypoint.sh redis-server --requirepass \"$REDIS_PASSWORD\" --save 60 1 --dir /data'",
    healthcheck: "redis-cli -h 127.0.0.1 -a \"$REDIS_PASSWORD\" --no-auth-warning ping | grep -q PONG",
    env: {
      REDIS_PASSWORD: GENERATED_PASSWORD, REDISUSER: "default", REDISPASSWORD: "${{ REDIS_PASSWORD }}", REDISHOST: HOST,
      REDISPORT: "6379", REDIS_URL: "redis://${{ REDISUSER }}:${{ REDIS_PASSWORD }}@${{ REDISHOST }}:${{ REDISPORT }}",
    },
  },
  {
    id: "mongodb", label: "MongoDB", image: "mongo:8.0", dataPath: "/data/db",
    startCommand: "docker-entrypoint.sh mongod --ipv6 --bind_ip ::,0.0.0.0 --setParameter diagnosticDataCollectionEnabled=false",
    healthcheck: "mongosh --quiet --host 127.0.0.1 --eval \"db.adminCommand('ping').ok\" | grep -q 1",
    env: {
      MONGO_INITDB_ROOT_USERNAME: "mongo", MONGO_INITDB_ROOT_PASSWORD: GENERATED_PASSWORD,
      MONGOUSER: "${{ MONGO_INITDB_ROOT_USERNAME }}", MONGOPASSWORD: "${{ MONGO_INITDB_ROOT_PASSWORD }}",
      MONGOHOST: HOST, MONGOPORT: "27017",
      MONGO_URL: `mongodb://\${{ MONGO_INITDB_ROOT_USERNAME }}:\${{ MONGO_INITDB_ROOT_PASSWORD }}@${HOST}:27017`,
    },
  },
  {
    id: "mysql", label: "MySQL", image: "mysql:9.4", dataPath: "/var/lib/mysql",
    startCommand: "docker-entrypoint.sh mysqld --innodb-use-native-aio=0 --disable-log-bin --performance_schema=0",
    healthcheck: "mysqladmin ping -h 127.0.0.1 -uroot -p\"$MYSQL_ROOT_PASSWORD\" --silent",
    env: {
      MYSQL_ROOT_PASSWORD: GENERATED_PASSWORD, MYSQL_DATABASE: "ployz",
      MYSQLUSER: "root", MYSQLPASSWORD: "${{ MYSQL_ROOT_PASSWORD }}", MYSQLDATABASE: "${{ MYSQL_DATABASE }}",
      MYSQLHOST: HOST, MYSQLPORT: "3306",
      MYSQL_URL: `mysql://\${{ MYSQLUSER }}:\${{ MYSQL_ROOT_PASSWORD }}@${HOST}:3306/\${{ MYSQL_DATABASE }}`,
    },
  },
];

/** A Service Template as people read it: its preset's label, else its id. */
export const templateLabel = ({ id }: { id: string }) => DATABASE_PRESETS.find((preset) => preset.id === id)?.label ?? id;


/**
 * One Batch that creates the preset's Service and its Volume (managed, at the default limit, like any new Volume)
 * mounted at its data path, then sets its start command, healthcheck (at the default timeout) and variables. All of it
 * saves, or none.
 */
export function databaseCommand(preset: DatabasePreset, target: {
  service: string; volume: string; environment: EnvironmentRef; name: string; volumeName: string;
}): ConfigCommand {
  const { service, volume, environment, name, volumeName } = target;
  const env = Object.fromEntries(Object.entries(preset.env)
    .map(([key, value]) => [key, { value, exported: true }]));
  const settings = { healthcheck: { command: preset.healthcheck }, env };
  return {
    command: "batch",
    environment,
    commands: [
      // The Store checks the ids are UUIDs.
      { command: "create_service", id: service, environment, name, image: preset.image, template: { id: preset.id, version: 1 } },
      { command: "create_volume", id: volume, environment, name: volumeName,
        storage: { kind: "provisioned", maximumBytes: Number(DEFAULT_VOLUME_GB) * 1_000_000_000 }, mounts: [{ service: name, path: preset.dataPath }] },
      { command: "edit", environment, expect: null, changes: [{ op: "patch", path: name,
        value: preset.startCommand === undefined ? settings : { startCommand: preset.startCommand, ...settings } }] },
    ],
  };
}
