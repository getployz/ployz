#!/usr/bin/env python3
"""Disposable stock-Corrosion v1 API driver for the three-node spike."""

from __future__ import annotations

import argparse
import datetime
import http.client
import json
import math
import os
import pathlib
import socket
import sys
import time
import urllib.parse
import uuid
from typing import Any, Iterable


TABLES = ("cluster_intent", "route_state", "machine_facts")
CHURN_LABELS = (
    "machine-join",
    "machine-accept",
    "route-attach",
    "serving-promote",
    "fact-refresh",
    "machine-drain",
    "machine-remove",
)


class ApiError(RuntimeError):
    pass


def wall_time() -> tuple[int, str]:
    timestamp_ns = time.time_ns()
    iso = datetime.datetime.fromtimestamp(
        timestamp_ns / 1_000_000_000, datetime.timezone.utc
    ).isoformat(timespec="microseconds")
    return timestamp_ns, iso.replace("+00:00", "Z")


def emit(value: Any) -> None:
    print(json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=True))


class CorrosionApi:
    def __init__(self, address: str, token_file: pathlib.Path, timeout: float):
        host, separator, port_text = address.rpartition(":")
        if not separator or not host:
            raise ValueError("--api must be HOST:PORT")
        self.host = host
        self.port = int(port_text)
        self.timeout = timeout
        token = token_file.read_text(encoding="utf-8").strip()
        if not token:
            raise ValueError("bearer token file is empty")
        self._authorization = f"Bearer {token}"

    def _connection(self, timeout: float | None = None) -> http.client.HTTPConnection:
        return http.client.HTTPConnection(
            self.host, self.port, timeout=self.timeout if timeout is None else timeout
        )

    def _headers(self) -> dict[str, str]:
        return {
            "Accept": "application/json",
            "Authorization": self._authorization,
            "Content-Type": "application/json",
        }

    def request_json(
        self, method: str, path: str, body: Any | None = None
    ) -> tuple[Any, dict[str, str]]:
        connection = self._connection()
        encoded = None if body is None else json.dumps(body, separators=(",", ":"))
        try:
            connection.request(method, path, body=encoded, headers=self._headers())
            response = connection.getresponse()
            raw = response.read()
            headers = {name.lower(): value for name, value in response.getheaders()}
            if not 200 <= response.status < 300:
                detail = raw.decode("utf-8", errors="replace")
                raise ApiError(f"{method} {path} returned HTTP {response.status}: {detail}")
            if not raw:
                return None, headers
            return json.loads(raw), headers
        finally:
            connection.close()

    def stream(
        self, method: str, path: str, body: Any | None, timeout: float
    ) -> tuple[http.client.HTTPConnection, http.client.HTTPResponse, dict[str, str]]:
        connection = self._connection(timeout)
        encoded = None if body is None else json.dumps(body, separators=(",", ":"))
        connection.request(method, path, body=encoded, headers=self._headers())
        response = connection.getresponse()
        headers = {name.lower(): value for name, value in response.getheaders()}
        if not 200 <= response.status < 300:
            raw = response.read().decode("utf-8", errors="replace")
            connection.close()
            raise ApiError(f"{method} {path} returned HTTP {response.status}: {raw}")
        return connection, response, headers

    def transaction(self, statements: list[Any]) -> Any:
        response, _ = self.request_json("POST", "/v1/transactions", statements)
        if not isinstance(response, dict):
            raise ApiError("transaction response was not a JSON object")
        errors = [
            result["error"]
            for result in response.get("results", [])
            if isinstance(result, dict) and "error" in result
        ]
        if errors:
            raise ApiError("transaction failed: " + "; ".join(str(error) for error in errors))
        return response

    def query_events(self, sql: str, params: list[Any] | None = None) -> list[dict[str, Any]]:
        body: Any = sql if params is None else [sql, params]
        connection, response, _ = self.stream(
            "POST", "/v1/queries", body, self.timeout
        )
        events: list[dict[str, Any]] = []
        try:
            while True:
                raw = response.readline()
                if not raw:
                    break
                event = json.loads(raw)
                if not isinstance(event, dict):
                    raise ApiError("query stream contained a non-object event")
                events.append(event)
        finally:
            connection.close()
        return events

    def health(self) -> Any:
        response, _ = self.request_json("GET", "/v1/health")
        return response


def api_from_args(args: argparse.Namespace) -> CorrosionApi:
    return CorrosionApi(args.api, args.token_file, args.timeout_seconds)


def statement(sql: str, params: list[Any]) -> list[Any]:
    return [sql, params]


def record_id(prefix: str) -> str:
    return f"{prefix}-{uuid.uuid4().hex}"


def document(
    identifier: str,
    kind: str,
    writer_id: str,
    extra: dict[str, Any] | None = None,
) -> tuple[dict[str, Any], int]:
    timestamp_ns, timestamp = wall_time()
    value: dict[str, Any] = {
        "id": identifier,
        "kind": kind,
        "writer_id": writer_id,
        "writer_timestamp": timestamp,
        "writer_timestamp_ns": timestamp_ns,
    }
    if extra:
        value.update(extra)
    return value, timestamp_ns


def insert_document(table: str, identifier: str, value: dict[str, Any]) -> list[Any]:
    return statement(
        f"INSERT INTO {table} (id, document) VALUES (?, ?)",
        [identifier, json.dumps(value, sort_keys=True, separators=(",", ":"))],
    )


def cmd_write_idle(args: argparse.Namespace) -> dict[str, Any]:
    api = api_from_args(args)
    if args.id and len(args.id) != args.count:
        raise ValueError("the number of --id values must match --count")
    writes = []
    for index in range(args.count):
        identifier = args.id[index] if args.id else record_id(f"idle-{args.writer_id}")
        value, timestamp_ns = document(
            identifier,
            "idle_sample",
            args.writer_id,
            {
                "machine_id": args.machine_id,
                "observed_at_ms": time.time_ns() // 1_000_000,
                "sequence": index,
            },
        )
        if args.schema_generation is None:
            insert = insert_document("machine_facts", identifier, value)
        else:
            insert = statement(
                "INSERT INTO machine_facts (id, document, schema_generation) "
                "VALUES (?, ?, ?)",
                [
                    identifier,
                    json.dumps(value, sort_keys=True, separators=(",", ":")),
                    args.schema_generation,
                ],
            )
        response = api.transaction([insert])
        writes.append(
            {
                "id": identifier,
                "response": response,
                "writer_timestamp_ns": timestamp_ns,
            }
        )
        if index + 1 < args.count:
            time.sleep(args.interval_ms / 1000)
    return {
        "command": "write-idle",
        "count": len(writes),
        "ids": [write["id"] for write in writes],
        "ok": True,
        "writes": writes,
    }


def cmd_write_deploy(args: argparse.Namespace) -> dict[str, Any]:
    api = api_from_args(args)
    if args.burst_id and len(args.burst_id) != args.bursts:
        raise ValueError("the number of --burst-id values must match --bursts")
    bursts = []
    for sequence in range(args.bursts):
        burst_id = (
            args.burst_id[sequence]
            if args.burst_id
            else record_id(f"deploy-{args.writer_id}")
        )
        if args.burst_id:
            intent_id = f"{burst_id}-intent"
            route_id = f"{burst_id}-route"
            serving_id = f"{burst_id}-serving"
            fact_id = f"{burst_id}-fact"
        else:
            intent_id = record_id("namespace-revision")
            route_id = record_id("route-binding")
            serving_id = record_id("serving-target")
            fact_id = record_id("deploy-fact")
        intent, started_ns = document(
            intent_id,
            "namespace_revision",
            args.writer_id,
            {
                "burst_id": burst_id,
                "namespace_id": args.namespace_id,
                "recorded_at_ms": time.time_ns() // 1_000_000,
                "sequence": sequence,
            },
        )
        route, _ = document(
            route_id,
            "route_binding",
            args.writer_id,
            {
                "burst_id": burst_id,
                "hostname": f"{burst_id}.spike.invalid",
                "namespace_id": args.namespace_id,
                "service_id": args.service_id,
            },
        )
        serving, _ = document(
            serving_id,
            "serving_target",
            args.writer_id,
            {
                "burst_id": burst_id,
                "namespace_id": args.namespace_id,
                "service_id": args.service_id,
            },
        )
        fact, _ = document(
            fact_id,
            "deploy_observation",
            args.writer_id,
            {
                "burst_id": burst_id,
                "machine_id": args.machine_id,
                "observed_at_ms": time.time_ns() // 1_000_000,
            },
        )
        response = api.transaction(
            [
                insert_document("cluster_intent", intent_id, intent),
                insert_document("route_state", route_id, route),
                insert_document("route_state", serving_id, serving),
                insert_document("machine_facts", fact_id, fact),
            ]
        )
        bursts.append(
            {
                "burst_id": burst_id,
                "ids": {
                    "cluster_intent": [intent_id],
                    "machine_facts": [fact_id],
                    "route_state": [route_id, serving_id],
                },
                "response": response,
                "writer_timestamp_ns": started_ns,
            }
        )
        if sequence + 1 < args.bursts:
            time.sleep(args.interval_ms / 1000)
    return {
        "bursts": bursts,
        "command": "write-deploy",
        "count": len(bursts),
        "ok": True,
    }


def decode_document(row_id: Any, raw_document: Any) -> tuple[dict[str, Any] | None, str | None]:
    if not isinstance(raw_document, str) or not raw_document.strip():
        return None, "empty_document"
    try:
        parsed = json.loads(raw_document)
    except json.JSONDecodeError:
        return None, "invalid_json"
    if not isinstance(parsed, dict):
        return None, "document_not_object"
    if not parsed.get("id") or not parsed.get("kind"):
        return None, "partial_document"
    if str(parsed["id"]) != str(row_id):
        return None, "id_mismatch"
    return parsed, None


def rows_from_events(events: Iterable[dict[str, Any]]) -> dict[str, Any]:
    columns: list[str] | None = None
    valid = []
    invalid = []
    eoq: dict[str, Any] | None = None
    errors = []
    for event in events:
        if "columns" in event:
            columns = event["columns"]
        elif "row" in event:
            row_event = event["row"]
            if (
                not isinstance(row_event, list)
                or len(row_event) != 2
                or not isinstance(row_event[1], list)
            ):
                invalid.append({"reason": "malformed_row_event", "raw": event})
                continue
            values = row_event[1]
            if not columns or len(columns) != len(values):
                invalid.append({"reason": "column_value_mismatch", "raw": event})
                continue
            row = dict(zip(columns, values))
            parsed, reason = decode_document(row.get("id"), row.get("document"))
            if reason:
                invalid.append(
                    {
                        "id": row.get("id"),
                        "raw_document": row.get("document"),
                        "reason": reason,
                    }
                )
                continue
            valid.append(
                {
                    "document": parsed,
                    "id": row["id"],
                    "raw_document": row["document"],
                    "row_id": row_event[0],
                }
            )
        elif "eoq" in event:
            eoq = event["eoq"]
        elif "error" in event:
            errors.append(event["error"])
    return {
        "errors": errors,
        "invalid_count": len(invalid),
        "invalid_rows": invalid,
        "query_eoq": eoq,
        "valid_count": len(valid),
        "valid_rows": valid,
    }


def query_documents(api: CorrosionApi, table: str, ids: list[str] | None = None) -> dict[str, Any]:
    if ids:
        placeholders = ",".join("?" for _ in ids)
        sql = (
            f"SELECT id, document FROM {table} "
            f"WHERE id IN ({placeholders}) ORDER BY id"
        )
        events = api.query_events(sql, ids)
    else:
        events = api.query_events(f"SELECT id, document FROM {table} ORDER BY id")
    result = rows_from_events(events)
    result["raw_events"] = events
    return result


def percentile(samples: list[float], quantile: float) -> float | None:
    if not samples:
        return None
    ordered = sorted(samples)
    if len(ordered) == 1:
        return ordered[0]
    position = (len(ordered) - 1) * quantile
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    return ordered[lower] + (ordered[upper] - ordered[lower]) * (position - lower)


def cmd_observe(args: argparse.Namespace) -> dict[str, Any]:
    api = api_from_args(args)
    expected = sorted(set(args.expected_id))
    deadline = time.monotonic() + args.wait_seconds
    found: dict[str, dict[str, Any]] = {}
    invalid_by_id: dict[str, dict[str, Any]] = {}
    polls = 0
    last_raw_events: list[dict[str, Any]] = []
    while True:
        polls += 1
        result = query_documents(api, args.table, expected)
        last_raw_events = result["raw_events"]
        observer_ns, observer_timestamp = wall_time()
        for row in result["valid_rows"]:
            if row["id"] not in found:
                row["observer_timestamp"] = observer_timestamp
                row["observer_timestamp_ns"] = observer_ns
                found[row["id"]] = row
        for row in result["invalid_rows"]:
            identifier = str(row.get("id"))
            invalid_by_id[identifier] = row
        if all(identifier in found for identifier in expected):
            break
        if time.monotonic() >= deadline:
            break
        time.sleep(args.poll_interval_ms / 1000)

    samples = []
    for identifier in expected:
        row = found.get(identifier)
        if not row:
            continue
        writer_ns = row["document"].get("writer_timestamp_ns")
        if not isinstance(writer_ns, int):
            invalid_by_id[identifier] = {
                "id": identifier,
                "raw_document": row["raw_document"],
                "reason": "missing_writer_timestamp_ns",
            }
            continue
        raw_ms = (row["observer_timestamp_ns"] - writer_ns) / 1_000_000
        corrected_ms = (
            raw_ms - args.observer_residual_ms + args.writer_residual_ms
        )
        implausible = (
            None
            if args.implausible_after_ms is None
            else corrected_ms > args.implausible_after_ms
        )
        samples.append(
            {
                "corrected_ms": corrected_ms,
                "flags": {
                    "implausible": implausible,
                    "negative": corrected_ms < 0,
                },
                "id": identifier,
                "observer_timestamp": row["observer_timestamp"],
                "observer_timestamp_ns": row["observer_timestamp_ns"],
                "raw_document": row["raw_document"],
                "raw_ms": raw_ms,
                "writer_timestamp": row["document"].get("writer_timestamp"),
                "writer_timestamp_ns": writer_ns,
            }
        )
    corrected = [sample["corrected_ms"] for sample in samples]
    return {
        "clock_correction": {
            "formula": "raw_ms - observer_residual_ms + writer_residual_ms",
            "implausible_after_ms": args.implausible_after_ms,
            "observer_residual_ms": args.observer_residual_ms,
            "writer_residual_ms": args.writer_residual_ms,
        },
        "command": "observe",
        "expected_ids": expected,
        "found_ids": sorted(found),
        "invalid_count": len(invalid_by_id),
        "invalid_rows": [invalid_by_id[key] for key in sorted(invalid_by_id)],
        "latency_ms": {
            "p50": percentile(corrected, 0.50),
            "p99": percentile(corrected, 0.99),
            "quantile_method": "linear interpolation over sorted samples",
            "raw_samples": samples,
        },
        "missing_ids": sorted(set(expected) - set(found)),
        "ok": all(identifier in found for identifier in expected),
        "polls": polls,
        "raw_query_events": last_raw_events,
        "table": args.table,
    }


def cmd_subscription(args: argparse.Namespace) -> dict[str, Any]:
    api = api_from_args(args)
    sql = f"SELECT id, document FROM {args.table} ORDER BY id"
    if args.resume_id:
        parsed_id = str(uuid.UUID(args.resume_id))
        path = f"/v1/subscriptions/{urllib.parse.quote(parsed_id)}"
        query = {"skip_rows": "true"}
        if args.from_change_id is not None:
            query["from"] = str(args.from_change_id)
        path += "?" + urllib.parse.urlencode(query)
        method = "GET"
        body = None
    else:
        path = "/v1/subscriptions?skip_rows=false"
        method = "POST"
        body = sql

    output: dict[str, Any] = {
        "change_ids": [],
        "command": "subscription",
        "fatal": None,
        "folds": [],
        "from_change_id": args.from_change_id,
        "ok": True,
        "query_id": args.resume_id,
        "restart_resnapshot_required": False,
        "resume_required": False,
        "table": args.table,
        "window_loss": None,
    }
    try:
        connection, response, headers = api.stream(
            method, path, body, args.duration_seconds
        )
    except (ApiError, OSError) as error:
        output.update(
            {
                "fatal": f"subscription_open_failed: {error}",
                "ok": False,
                "restart_resnapshot_required": True,
            }
        )
        return output

    output["query_id"] = headers.get("corro-query-id", output["query_id"])
    output["query_hash"] = headers.get("corro-query-hash")
    deadline = time.monotonic() + args.duration_seconds
    last_change_id = args.from_change_id
    observed_eoq = args.from_change_id is not None
    changes = 0
    initial_events = []
    stream_ended = False
    try:
        while changes < args.max_changes:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            if connection.sock is not None:
                connection.sock.settimeout(max(0.05, remaining))
            try:
                raw = response.readline()
            except (TimeoutError, socket.timeout):
                break
            if not raw:
                stream_ended = True
                break
            try:
                event = json.loads(raw)
            except json.JSONDecodeError:
                output["fatal"] = "subscription_stream_invalid_json"
                output["ok"] = False
                output["restart_resnapshot_required"] = True
                break
            if not isinstance(event, dict):
                output["fatal"] = "subscription_stream_non_object_event"
                output["ok"] = False
                output["restart_resnapshot_required"] = True
                break
            if "error" in event:
                output["fatal"] = str(event["error"])
                output["ok"] = False
                output["restart_resnapshot_required"] = True
                break
            if "eoq" in event:
                observed_eoq = True
                candidate = event["eoq"].get("change_id")
                if candidate is not None:
                    last_change_id = int(candidate)
                initial_events.append(event)
                output["initial_snapshot"] = rows_from_events(initial_events)
                continue
            if not observed_eoq:
                initial_events.append(event)
            if "change" not in event:
                continue
            change = event["change"]
            if not isinstance(change, list) or len(change) != 4:
                output["fatal"] = "malformed_change_event"
                output["ok"] = False
                output["restart_resnapshot_required"] = True
                break
            change_id = int(change[3])
            expected = None if last_change_id is None else last_change_id + 1
            if expected is not None and change_id != expected:
                output["window_loss"] = {
                    "expected_change_id": expected,
                    "observed_change_id": change_id,
                }
                output["ok"] = False
                output["restart_resnapshot_required"] = True
                break
            last_change_id = change_id
            output["change_ids"].append(change_id)
            fold = query_documents(api, args.table)
            output["folds"].append(
                {
                    "change_id": change_id,
                    "errors": fold["errors"],
                    "ids": sorted(row["id"] for row in fold["valid_rows"]),
                    "invalid_count": fold["invalid_count"],
                    "invalid_rows": fold["invalid_rows"],
                    "valid_count": fold["valid_count"],
                }
            )
            changes += 1
    finally:
        connection.close()

    output["last_change_id"] = last_change_id
    output["stream_ended"] = stream_ended
    if stream_ended:
        output["resume_required"] = observed_eoq
        if not observed_eoq:
            output["ok"] = False
            output["restart_resnapshot_required"] = True
            output["fatal"] = "stream_ended_before_initial_snapshot"
    return output


def cmd_churn(args: argparse.Namespace) -> dict[str, Any]:
    api = api_from_args(args)
    labels = args.label or list(CHURN_LABELS)
    if len(labels) != 7:
        raise ValueError("churn requires exactly seven --label values")
    cycles = []
    for cycle, label in enumerate(labels, start=1):
        identifier = record_id(f"churn-{args.writer_id}")
        value, timestamp_ns = document(
            identifier,
            "machine_churn",
            args.writer_id,
            {
                "cycle": cycle,
                "label": label,
                "machine_id": args.machine_id,
                "observed_at_ms": time.time_ns() // 1_000_000,
            },
        )
        insert_response = api.transaction(
            [insert_document("machine_facts", identifier, value)]
        )
        delete_response = api.transaction(
            [statement("DELETE FROM machine_facts WHERE id = ?", [identifier])]
        )
        cycles.append(
            {
                "deleted_id": identifier,
                "insert_response": insert_response,
                "label": label,
                "remove_response": delete_response,
                "writer_timestamp_ns": timestamp_ns,
            }
        )
        if cycle < len(labels):
            time.sleep(args.interval_ms / 1000)
    return {
        "command": "churn",
        "cycles": cycles,
        "labels": labels,
        "never_reused_ids": [cycle["deleted_id"] for cycle in cycles],
        "ok": True,
    }


def scalar_rows(api: CorrosionApi, sql: str) -> list[list[Any]]:
    events = api.query_events(sql)
    rows = []
    errors = []
    for event in events:
        if "row" in event:
            rows.append(event["row"][1])
        elif "error" in event:
            errors.append(str(event["error"]))
    if errors:
        raise ApiError("; ".join(errors))
    return rows


def file_size(path: pathlib.Path) -> int | None:
    try:
        return path.stat().st_size
    except FileNotFoundError:
        return None


def process_metrics(pid: int) -> dict[str, Any]:
    stat_fields = pathlib.Path(f"/proc/{pid}/stat").read_text(encoding="utf-8").split()
    status = pathlib.Path(f"/proc/{pid}/status").read_text(encoding="utf-8").splitlines()
    status_values = {}
    for line in status:
        if ":" in line:
            key, value = line.split(":", 1)
            status_values[key] = value.strip()
    ticks_per_second = os.sysconf(os.sysconf_names["SC_CLK_TCK"])
    uptime_seconds = float(pathlib.Path("/proc/uptime").read_text().split()[0])
    user_seconds = int(stat_fields[13]) / ticks_per_second
    system_seconds = int(stat_fields[14]) / ticks_per_second
    elapsed_seconds = max(0.0, uptime_seconds - int(stat_fields[21]) / ticks_per_second)
    cpu_seconds = user_seconds + system_seconds
    return {
        "cpu_percent_lifetime": (
            None if elapsed_seconds == 0 else cpu_seconds / elapsed_seconds * 100
        ),
        "elapsed_seconds": elapsed_seconds,
        "pid": pid,
        "rss": status_values.get("VmRSS"),
        "system_cpu_seconds": system_seconds,
        "user_cpu_seconds": user_seconds,
    }


def cmd_collect(args: argparse.Namespace) -> dict[str, Any]:
    api = api_from_args(args)
    clock_pk = {}
    for table in TABLES:
        rows = scalar_rows(
            api,
            "SELECT "
            f"(SELECT COUNT(*) FROM {table}__crsql_clock), "
            f"(SELECT COUNT(*) FROM {table}__crsql_pks)",
        )
        if len(rows) != 1 or len(rows[0]) != 2:
            raise ApiError(f"unexpected internal count result for {table}")
        clock_pk[table] = {"clock_rows": rows[0][0], "pk_rows": rows[0][1]}

    version_rows = scalar_rows(
        api,
        "SELECT hex(site_id), db_version "
        "FROM crsql_db_versions ORDER BY hex(site_id)",
    )
    gap_rows = scalar_rows(
        api,
        "SELECT hex(actor_id), start, end "
        "FROM __corro_bookkeeping_gaps ORDER BY hex(actor_id), start, end",
    )
    gap_count = sum(int(row[2]) - int(row[1]) + 1 for row in gap_rows)
    pid = args.pid
    if args.pid_file is not None:
        pid = int(args.pid_file.read_text(encoding="utf-8").strip())
    db_path = args.db_path
    try:
        health = {"response": api.health(), "reachable": True}
    except ApiError as error:
        health = {"error": str(error), "reachable": False}
    return {
        "clock_pk_counts": clock_pk,
        "command": "collect",
        "gaps": {
            "missing_version_count": gap_count,
            "ranges": [
                {"actor_id": row[0], "end": row[2], "start": row[1]}
                for row in gap_rows
            ],
        },
        "health": health,
        "ok": True,
        "process": None if pid is None else process_metrics(pid),
        "storage_bytes": {
            "db": file_size(db_path),
            "shm": file_size(pathlib.Path(str(db_path) + "-shm")),
            "wal": file_size(pathlib.Path(str(db_path) + "-wal")),
        },
        "version_vector": [
            {"actor_id": row[0], "db_version": row[1]} for row in version_rows
        ],
    }


def nested_values(value: Any, key: str) -> Iterable[Any]:
    if isinstance(value, dict):
        for candidate_key, candidate in value.items():
            if candidate_key == key:
                yield candidate
            yield from nested_values(candidate, key)
    elif isinstance(value, list):
        for item in value:
            yield from nested_values(item, key)


def markdown_cell(value: Any) -> str:
    return str(value).replace("|", "\\|").replace("\n", " ")


def cmd_report(args: argparse.Namespace) -> dict[str, Any]:
    evidence_dir = args.evidence_dir.resolve()
    entries = []
    unreadable = []
    for path in sorted(evidence_dir.rglob("*.json")):
        if path == evidence_dir / "report.json":
            continue
        try:
            entries.append((path.relative_to(evidence_dir), json.loads(path.read_text())))
        except (OSError, json.JSONDecodeError) as error:
            unreadable.append((path.relative_to(evidence_dir), str(error)))

    operator_steps = []
    limitations = []
    restart_resnapshots = 0
    fatal_streams = 0
    window_losses = 0
    latency_summaries = []
    storage_summaries = []
    for relative, value in entries:
        for steps in nested_values(value, "operator_steps"):
            if isinstance(steps, list):
                for step in steps:
                    operator_steps.append((relative, step))
        for found in nested_values(value, "limitations"):
            if isinstance(found, list):
                limitations.extend(str(item) for item in found)
            elif found:
                limitations.append(str(found))
        restart_resnapshots += sum(
            item is True for item in nested_values(value, "restart_resnapshot_required")
        )
        fatal_streams += sum(
            item not in (None, False, "") for item in nested_values(value, "fatal")
        )
        window_losses += sum(
            item not in (None, False, "") for item in nested_values(value, "window_loss")
        )
        for summary in nested_values(value, "latency_ms"):
            if isinstance(summary, dict):
                latency_summaries.append((relative, summary))
        for summary in nested_values(value, "storage_bytes"):
            if isinstance(summary, dict):
                storage_summaries.append((relative, summary))

    recovery_steps = []
    for relative, step in operator_steps:
        if isinstance(step, dict) and (
            step.get("recovery") is True or step.get("category") == "recovery"
        ):
            recovery_steps.append((relative, step))

    fixed_limitations = [
        "This is a disposable three-node spike with synthetic Ployz-shaped rows, not a production row-model decision.",
        "Wall-clock latency depends on the supplied chrony residuals; no latency pass threshold is inferred.",
        "Subscription deltas are gap evidence and invalidation only; every reported fold comes from a full query.",
        "Results describe the exercised Corrosion v1.0.0 build and host topology, not all failure modes.",
    ]
    all_limitations = list(dict.fromkeys(limitations + fixed_limitations))
    lines = [
        "# Corrosion spike evidence",
        "",
        "## Operator burden and recovery",
        "",
        f"- Evidence JSON files read: {len(entries)}; unreadable: {len(unreadable)}.",
        f"- Explicit operator steps recorded: {len(operator_steps)}.",
        f"- Explicit recovery commands recorded: {len(recovery_steps)}.",
        f"- Restart plus resnapshot outcomes: {restart_resnapshots}; fatal subscription outcomes: {fatal_streams}; window-loss outcomes: {window_losses}.",
        "- Recovery counts use only steps explicitly labelled `recovery: true` or `category: recovery`; shell text is not guessed.",
        "",
    ]
    if recovery_steps:
        lines.extend(
            [
                "| Evidence | Recovery command |",
                "| --- | --- |",
            ]
        )
        for relative, step in recovery_steps:
            command = (
                step.get("command")
                if isinstance(step, dict)
                else str(step)
            )
            lines.append(
                f"| {markdown_cell(relative)} | {markdown_cell(command)} |"
            )
        lines.append("")

    lines.extend(
        [
            "## Phase evidence",
            "",
            "| Evidence | Command | Result |",
            "| --- | --- | --- |",
        ]
    )
    for relative, value in entries:
        command = value.get("command", "phase") if isinstance(value, dict) else "unknown"
        ok = value.get("ok", "not recorded") if isinstance(value, dict) else "not recorded"
        lines.append(
            f"| {markdown_cell(relative)} | {markdown_cell(command)} | {markdown_cell(ok)} |"
        )
    if not entries:
        lines.append("| none | none | no JSON evidence supplied |")
    lines.append("")

    lines.extend(["## Replication latency", ""])
    if latency_summaries:
        lines.extend(
            [
                "| Evidence | Samples | p50 ms | p99 ms |",
                "| --- | ---: | ---: | ---: |",
            ]
        )
        for relative, summary in latency_summaries:
            samples = summary.get("raw_samples", [])
            lines.append(
                f"| {markdown_cell(relative)} | {len(samples)} | "
                f"{markdown_cell(summary.get('p50'))} | {markdown_cell(summary.get('p99'))} |"
            )
    else:
        lines.append("No latency samples were supplied.")
    lines.append("")

    lines.extend(["## Storage snapshots", ""])
    if storage_summaries:
        lines.extend(
            [
                "| Evidence | DB bytes | WAL bytes | SHM bytes |",
                "| --- | ---: | ---: | ---: |",
            ]
        )
        for relative, summary in storage_summaries:
            lines.append(
                f"| {markdown_cell(relative)} | {markdown_cell(summary.get('db'))} | "
                f"{markdown_cell(summary.get('wal'))} | {markdown_cell(summary.get('shm'))} |"
            )
    else:
        lines.append("No storage snapshots were supplied.")
    lines.append("")

    lines.extend(["## Limitations", ""])
    lines.extend(f"- {limitation}" for limitation in all_limitations)
    if unreadable:
        lines.append("")
        lines.append("Unreadable evidence:")
        lines.extend(
            f"- `{relative}`: {error}" for relative, error in unreadable
        )
    report_path = evidence_dir / "report.md"
    report_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return {
        "command": "report",
        "evidence_files": len(entries),
        "ok": not unreadable,
        "recovery_command_count": len(recovery_steps),
        "report": str(report_path),
        "unreadable_files": [str(relative) for relative, _ in unreadable],
    }


def add_api_args(parser: argparse.ArgumentParser) -> None:
    parser.add_argument("--api", default="127.0.0.1:8080")
    parser.add_argument("--token-file", type=pathlib.Path, required=True)
    parser.add_argument("--timeout-seconds", type=float, default=15)


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    idle = subparsers.add_parser("write-idle")
    add_api_args(idle)
    idle.add_argument("--writer-id", required=True)
    idle.add_argument("--machine-id", required=True)
    idle.add_argument("--count", type=int, default=1)
    idle.add_argument("--id", action="append")
    idle.add_argument("--interval-ms", type=int, default=1000)
    idle.add_argument("--schema-generation", type=int)
    idle.set_defaults(run=cmd_write_idle)

    deploy = subparsers.add_parser("write-deploy")
    add_api_args(deploy)
    deploy.add_argument("--writer-id", required=True)
    deploy.add_argument("--machine-id", required=True)
    deploy.add_argument("--namespace-id", default="namespace-spike")
    deploy.add_argument("--service-id", default="service-spike")
    deploy.add_argument("--bursts", type=int, default=1)
    deploy.add_argument("--burst-id", action="append")
    deploy.add_argument("--interval-ms", type=int, default=1000)
    deploy.set_defaults(run=cmd_write_deploy)

    observe = subparsers.add_parser("observe")
    add_api_args(observe)
    observe.add_argument("--table", choices=TABLES, required=True)
    observe.add_argument("--expected-id", action="append", required=True)
    observe.add_argument("--wait-seconds", type=float, default=30)
    observe.add_argument("--poll-interval-ms", type=int, default=100)
    observe.add_argument("--writer-residual-ms", type=float, required=True)
    observe.add_argument("--observer-residual-ms", type=float, required=True)
    observe.add_argument("--implausible-after-ms", type=float)
    observe.set_defaults(run=cmd_observe)

    subscription = subparsers.add_parser("subscription")
    add_api_args(subscription)
    subscription.add_argument("--table", choices=TABLES, required=True)
    subscription.add_argument("--resume-id")
    subscription.add_argument("--from-change-id", type=int)
    subscription.add_argument("--duration-seconds", type=float, default=30)
    subscription.add_argument("--max-changes", type=int, default=100)
    subscription.set_defaults(run=cmd_subscription)

    churn = subparsers.add_parser("churn")
    add_api_args(churn)
    churn.add_argument("--writer-id", required=True)
    churn.add_argument("--machine-id", required=True)
    churn.add_argument("--label", action="append")
    churn.add_argument("--interval-ms", type=int, default=1000)
    churn.set_defaults(run=cmd_churn)

    collect = subparsers.add_parser("collect")
    add_api_args(collect)
    collect.add_argument("--db-path", type=pathlib.Path, required=True)
    pid_source = collect.add_mutually_exclusive_group()
    pid_source.add_argument("--pid", type=int)
    pid_source.add_argument("--pid-file", type=pathlib.Path)
    collect.set_defaults(run=cmd_collect)

    report = subparsers.add_parser("report")
    report.add_argument("--evidence-dir", type=pathlib.Path, required=True)
    report.set_defaults(run=cmd_report)

    return parser


def validate_args(args: argparse.Namespace) -> None:
    for name in (
        "bursts",
        "count",
        "duration_seconds",
        "interval_ms",
        "max_changes",
        "poll_interval_ms",
        "timeout_seconds",
        "wait_seconds",
    ):
        value = getattr(args, name, None)
        if value is not None and value <= 0:
            raise ValueError(f"--{name.replace('_', '-')} must be positive")
    if getattr(args, "from_change_id", None) is not None and not getattr(
        args, "resume_id", None
    ):
        raise ValueError("--from-change-id requires --resume-id")
    if (
        getattr(args, "schema_generation", None) is not None
        and args.schema_generation < 1
    ):
        raise ValueError("--schema-generation must be positive")


def main() -> int:
    parser = build_parser()
    args = parser.parse_args()
    try:
        validate_args(args)
        result = args.run(args)
        emit(result)
        return 0 if result.get("ok", False) else 1
    except Exception as error:
        emit(
            {
                "command": args.command,
                "error": f"{type(error).__name__}: {error}",
                "ok": False,
            }
        )
        return 1


if __name__ == "__main__":
    sys.exit(main())
