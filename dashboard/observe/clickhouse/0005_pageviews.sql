-- Pageviews, apart from the other requests. The Web view groups every counted pageview by visitor, and the wide request table
-- is ordered by time, so each read sorted all of them and paid for columns it never used. This table holds only the pageviews
-- (the view below defines one) and only the columns the figures read, ordered by visitor, so a visitor's pageviews sit together.
-- The key starts with OrgId like the request table, and a read still filters OrgId and Namespace.
-- Visitor stays NULL on a day the Machine held no salt, as in the request table, hence allow_nullable_key.
-- Source is the figures' source (UTM source, else ref, else referrer host) worked out once, here.
-- Ua is the user agent. The bot list is read from it at read time, as it is for the request table.
-- A replayed batch lands twice until a merge folds rows that share a key, and the key holds everything that names a request.
CREATE TABLE IF NOT EXISTS otel.ployz_pageview
(
    `OrgId` LowCardinality(String),
    `RetentionDays` UInt16,
    `Timestamp` DateTime64(6),
    `Namespace` LowCardinality(String),
    `Visitor` Nullable(UInt64),
    `Host` LowCardinality(String),
    `Path` String CODEC(ZSTD(1)),
    `RequestId` String CODEC(ZSTD(1)),
    `Ua` LowCardinality(String),
    `Language` LowCardinality(String),
    `Source` LowCardinality(String),
    `UtmSource` LowCardinality(String),
    `UtmMedium` LowCardinality(String),
    `UtmCampaign` LowCardinality(String),
    `UtmTerm` String CODEC(ZSTD(1)),
    `UtmContent` String CODEC(ZSTD(1))
)
ENGINE = ReplacingMergeTree
PARTITION BY toDate(Timestamp)
ORDER BY (OrgId, Namespace, Visitor, Timestamp, RequestId, Path)
TTL toDateTime(Timestamp) + toIntervalDay(RetentionDays)
SETTINGS index_granularity = 8192, ttl_only_drop_parts = 0, allow_nullable_key = 1
;

-- The view reads otel.ployz_request, not otel_logs. A pageview is then a filter over the typed columns the figures already
-- read, with OrgId, RetentionDays and the rest copied from the request row it came from. Reading otel_logs would parse the
-- attributes a second time and could drift from the request table. A replayed batch reaches both tables as the same rows,
-- so each folds them by its own key.
-- A pageview is a GET answered 2xx with HTML that a person's browser asked for, and not a prefetch or prerender the person never saw.
--
-- Backfill, for a database that holds request rows from before this file. Run it once by hand and never from this file, since
-- a second run lands every pageview twice until a merge folds them. Its filter is the view's.
--   INSERT INTO otel.ployz_pageview
--   SELECT OrgId, RetentionDays, Timestamp, Namespace, Visitor, Host, Path, RequestId, Ua, Language,
--     if(UtmSource != '', UtmSource, if(Ref != '', Ref, ReferrerHost)), UtmSource, UtmMedium, UtmCampaign, UtmTerm, UtmContent
--   FROM otel.ployz_request
--   WHERE Method = 'GET' AND Status >= 200 AND Status < 300 AND startsWith(lower(ContentType), 'text/html') AND NOT match(SecPurpose, '(?i)prefetch|prerender')
CREATE MATERIALIZED VIEW IF NOT EXISTS otel.ployz_pageview_mv TO otel.ployz_pageview AS
SELECT
    OrgId,
    RetentionDays,
    Timestamp,
    Namespace,
    Visitor,
    Host,
    Path,
    RequestId,
    Ua,
    Language,
    if(UtmSource != '', UtmSource, if(Ref != '', Ref, ReferrerHost)) AS Source,
    UtmSource,
    UtmMedium,
    UtmCampaign,
    UtmTerm,
    UtmContent
FROM otel.ployz_request
WHERE Method = 'GET' AND Status >= 200 AND Status < 300 AND startsWith(lower(ContentType), 'text/html') AND NOT match(SecPurpose, '(?i)prefetch|prerender')
;
