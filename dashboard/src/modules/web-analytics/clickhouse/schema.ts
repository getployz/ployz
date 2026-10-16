import { Effect } from "effect";
import { LONGEST_RETENTION_MONTHS } from "../model";
import type { ClickHouseService } from "./client";

export const TTL_DAYS = LONGEST_RETENTION_MONTHS * 31;

const DDL = [
  `CREATE TABLE IF NOT EXISTS pageview (
    environment_id String,
    attempt UInt64,
    ts DateTime64(3, 'UTC') CODEC(Delta, ZSTD(1)),
    visitor_id Int64,
    host LowCardinality(String),
    pathname String CODEC(ZSTD(3)),
    source LowCardinality(String),
    referrer_host Nullable(String),
    utm_source Nullable(String),
    utm_medium Nullable(String),
    utm_campaign Nullable(String),
    utm_term Nullable(String),
    utm_content Nullable(String),
    ref Nullable(String),
    language LowCardinality(Nullable(String)),
    flags Int16,
    retention_days UInt16
  )
  ENGINE = MergeTree
  PARTITION BY (retention_days, toYYYYMM(ts))
  ORDER BY (environment_id, attempt, toDate(ts), visitor_id, ts)
  TTL toDateTime(ts) + toIntervalDay(retention_days)
  SETTINGS ttl_only_drop_parts = 1`,
  `CREATE TABLE IF NOT EXISTS daily (
    environment_id String,
    day Date,
    attempt UInt64,
    dim LowCardinality(String),
    value String,
    pageviews UInt64,
    visitors UInt64,
    visits UInt64,
    bounces UInt64,
    total_duration UInt64
  )
  ENGINE = MergeTree
  PARTITION BY toYYYYMM(day)
  ORDER BY (environment_id, dim, day, value)
  TTL day + toIntervalDay(${TTL_DAYS})
  SETTINGS ttl_only_drop_parts = 1`,
  `CREATE TABLE IF NOT EXISTS session_hourly (
    environment_id String,
    day Date,
    attempt UInt64,
    hour DateTime('UTC'),
    pageviews UInt64,
    visitors UInt64,
    visits UInt64,
    bounces UInt64,
    total_duration UInt64
  )
  ENGINE = MergeTree
  PARTITION BY toYYYYMM(hour)
  ORDER BY (environment_id, hour)
  TTL hour + toIntervalDay(${TTL_DAYS})
  SETTINGS ttl_only_drop_parts = 1`,
];

export const ensureSchema = (clickhouse: ClickHouseService) => Effect.forEach(DDL, (ddl) => clickhouse.unchecked.command(ddl), { discard: true });
