-- Request analytics. A Machine's Collector ships each access-log line as a log record into otel_logs, marked ployz.stream=requests,
-- with the client address already hashed into ployz.request.visitor. This view lifts those records into a typed table.
-- Optional text is '' when absent. Visitor is NULL on a day the Machine held no salt. Delivery is at least once, so a replayed
-- batch can land twice until a merge folds rows that share a key.
CREATE TABLE IF NOT EXISTS otel.ployz_request
(
    `OrgId` LowCardinality(String),
    `RetentionDays` UInt16,
    `Timestamp` DateTime64(6),
    `Namespace` LowCardinality(String),
    `Service` LowCardinality(String),
    `Host` LowCardinality(String),
    `Method` LowCardinality(String),
    `Path` String CODEC(ZSTD(1)),
    `Status` UInt16,
    `DurationUs` UInt64,
    `BytesIn` UInt64,
    `BytesOut` UInt64,
    `RequestId` String CODEC(ZSTD(1)),
    `Ua` String CODEC(ZSTD(1)),
    `ReferrerHost` LowCardinality(String),
    `Language` LowCardinality(String),
    `ContentType` LowCardinality(String),
    `SecPurpose` LowCardinality(String),
    `Visitor` Nullable(UInt64),
    `UtmSource` String CODEC(ZSTD(1)),
    `UtmMedium` String CODEC(ZSTD(1)),
    `UtmCampaign` String CODEC(ZSTD(1)),
    `UtmTerm` String CODEC(ZSTD(1)),
    `UtmContent` String CODEC(ZSTD(1)),
    `Ref` String CODEC(ZSTD(1))
)
ENGINE = ReplacingMergeTree
PARTITION BY toDate(Timestamp)
ORDER BY (OrgId, Namespace, Timestamp, RequestId, Path)
TTL toDateTime(Timestamp) + toIntervalDay(RetentionDays)
SETTINGS index_granularity = 8192, ttl_only_drop_parts = 0
;

CREATE MATERIALIZED VIEW IF NOT EXISTS otel.ployz_request_mv TO otel.ployz_request AS
SELECT
    OrgId,
    RetentionDays,
    Timestamp,
    LogAttributes['ployz.request.namespace'] AS Namespace,
    LogAttributes['ployz.request.service'] AS Service,
    LogAttributes['server.address'] AS Host,
    LogAttributes['http.request.method'] AS Method,
    LogAttributes['url.path'] AS Path,
    toUInt16OrZero(LogAttributes['http.response.status_code']) AS Status,
    toUInt64OrZero(LogAttributes['ployz.request.duration_us']) AS DurationUs,
    toUInt64OrZero(LogAttributes['ployz.request.bytes_in']) AS BytesIn,
    toUInt64OrZero(LogAttributes['ployz.request.bytes_out']) AS BytesOut,
    LogAttributes['ployz.request.id'] AS RequestId,
    LogAttributes['user_agent.original'] AS Ua,
    LogAttributes['ployz.request.referrer_host'] AS ReferrerHost,
    LogAttributes['ployz.request.language'] AS Language,
    LogAttributes['ployz.request.content_type'] AS ContentType,
    LogAttributes['ployz.request.sec_purpose'] AS SecPurpose,
    if(length(LogAttributes['ployz.request.visitor']) = 64, reinterpretAsUInt64(reverse(unhex(substring(LogAttributes['ployz.request.visitor'], 1, 16)))), NULL) AS Visitor,
    LogAttributes['ployz.request.utm_source'] AS UtmSource,
    LogAttributes['ployz.request.utm_medium'] AS UtmMedium,
    LogAttributes['ployz.request.utm_campaign'] AS UtmCampaign,
    LogAttributes['ployz.request.utm_term'] AS UtmTerm,
    LogAttributes['ployz.request.utm_content'] AS UtmContent,
    LogAttributes['ployz.request.ref'] AS Ref
FROM otel.otel_logs
WHERE LogAttributes['ployz.stream'] = 'requests'
;
