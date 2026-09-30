import type { ComponentType, SVGProps } from "react";
import type { ConfigCommand, EnvironmentRef } from "@ployz/sdk";
import { MongoIcon, MysqlIcon, PostgresIcon, RedisIcon } from "#/components/icons/database-logos";

/**
 * A Database Preset: a common database set up as Railway sets it up (image, data path, start command, variables),
 * created as an ordinary image Service with a Volume. Variables reference each other as Railway's do, with
 * `PLOYZ_PRIVATE_DOMAIN` for `RAILWAY_PRIVATE_DOMAIN`, and all are exported so other Services can reference them.
 */
// ponytail: the password is only read when the database first initializes its Volume; changing it later needs the
// database's own tooling too. No rotation.
export type DatabasePreset = {
  id: "postgres" | "redis" | "mongodb" | "mysql";
  label: string;
  icon: ComponentType<SVGProps<SVGSVGElement>>;
  image: string;
  dataPath: string;
  startCommand?: string;
  env: (password: string) => Record<string, string>;
};

const HOST = "${{ PLOYZ_PRIVATE_DOMAIN }}";

export const DATABASE_PRESETS: readonly DatabasePreset[] = [
  {
    id: "postgres", label: "PostgreSQL", icon: PostgresIcon,
    image: "ghcr.io/railwayapp-templates/postgres-ssl:18", dataPath: "/var/lib/postgresql/data",
    env: (password) => ({
      POSTGRES_USER: "postgres", POSTGRES_PASSWORD: password, POSTGRES_DB: "ployz",
      PGDATA: "/var/lib/postgresql/data/pgdata", SSL_CERT_DAYS: "820",
      PGUSER: "${{ POSTGRES_USER }}", PGPASSWORD: "${{ POSTGRES_PASSWORD }}", PGDATABASE: "${{ POSTGRES_DB }}",
      PGHOST: HOST, PGPORT: "5432",
      DATABASE_URL: `postgresql://\${{ PGUSER }}:\${{ POSTGRES_PASSWORD }}@${HOST}:5432/\${{ PGDATABASE }}`,
    }),
  },
  {
    id: "redis", label: "Redis", icon: RedisIcon, image: "redis:8.2", dataPath: "/data",
    startCommand: "docker-entrypoint.sh redis-server --requirepass \"$REDIS_PASSWORD\" --save 60 1 --dir /data",
    env: (password) => ({
      REDIS_PASSWORD: password, REDISUSER: "default", REDISPASSWORD: "${{ REDIS_PASSWORD }}", REDISHOST: HOST,
      REDISPORT: "6379", REDIS_URL: "redis://${{ REDISUSER }}:${{ REDIS_PASSWORD }}@${{ REDISHOST }}:${{ REDISPORT }}",
    }),
  },
  {
    id: "mongodb", label: "MongoDB", icon: MongoIcon, image: "mongo:8.0", dataPath: "/data/db",
    startCommand: "docker-entrypoint.sh mongod --ipv6 --bind_ip ::,0.0.0.0 --setParameter diagnosticDataCollectionEnabled=false",
    env: (password) => ({
      MONGO_INITDB_ROOT_USERNAME: "mongo", MONGO_INITDB_ROOT_PASSWORD: password,
      MONGOUSER: "${{ MONGO_INITDB_ROOT_USERNAME }}", MONGOPASSWORD: "${{ MONGO_INITDB_ROOT_PASSWORD }}",
      MONGOHOST: HOST, MONGOPORT: "27017",
      MONGO_URL: `mongodb://\${{ MONGO_INITDB_ROOT_USERNAME }}:\${{ MONGO_INITDB_ROOT_PASSWORD }}@${HOST}:27017`,
    }),
  },
  {
    id: "mysql", label: "MySQL", icon: MysqlIcon, image: "mysql:9.4", dataPath: "/var/lib/mysql",
    startCommand: "docker-entrypoint.sh mysqld --innodb-use-native-aio=0 --disable-log-bin --performance_schema=0",
    env: (password) => ({
      MYSQL_ROOT_PASSWORD: password, MYSQL_DATABASE: "ployz",
      MYSQLUSER: "root", MYSQLPASSWORD: "${{ MYSQL_ROOT_PASSWORD }}", MYSQLDATABASE: "${{ MYSQL_DATABASE }}",
      MYSQLHOST: HOST, MYSQLPORT: "3306",
      MYSQL_URL: `mysql://\${{ MYSQLUSER }}:\${{ MYSQL_ROOT_PASSWORD }}@${HOST}:3306/\${{ MYSQL_DATABASE }}`,
    }),
  },
];

const LETTERS = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ";

/** 32 random letters, as Railway's `secret(32, a-zA-Z)`. */
export function randomPassword() {
  let password = "";
  while (password.length < 32) {
    for (const byte of crypto.getRandomValues(new Uint8Array(32))) {
      // 208 is the largest multiple of 52 below 256: rejecting the rest keeps every letter equally likely.
      if (byte < 208 && password.length < 32) password += LETTERS[byte % 52];
    }
  }
  return password;
}

/**
 * One Batch that creates the preset's Service and its Volume (managed, 5 GB, like any new Volume) mounted at its data
 * path, then sets its start command and variables. All of it saves, or none.
 */
export function databaseCommand(preset: DatabasePreset, target: {
  service: string; volume: string; environment: EnvironmentRef; name: string; volumeName: string; password: string;
}): ConfigCommand {
  const { service, volume, environment, name, volumeName } = target;
  const env = Object.fromEntries(Object.entries(preset.env(target.password))
    .map(([key, value]) => [key, { value, exported: true }]));
  return {
    command: "batch",
    commands: [
      // The Store checks the ids are UUIDs.
      { command: "create_service", id: service, environment, name, image: preset.image },
      { command: "create_volume", id: volume, environment, name: volumeName,
        storage: { kind: "provisioned", maximumBytes: 5_000_000_000 }, mounts: [{ service: name, path: preset.dataPath }] },
      { command: "edit", environment, expect: null, changes: [{ op: "patch", path: name,
        value: preset.startCommand === undefined ? { env } : { startCommand: preset.startCommand, env } }] },
    ],
  };
}
