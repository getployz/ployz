"""Numbered Postgres inserts through `ployz exec`; every acknowledged id is logged on the host.

On stop, the ids read back from the Service's current writer are compared with the acknowledged ones.
An id is acknowledged only after psql prints its RETURNING row, which follows the commit under
synchronous_commit=on. An id in flight when a session drops is never retried and never counted.
"""

import json
from pathlib import Path
import signal
import subprocess
import threading
import time

TABLE = "ackprobe"


def report(acks, present):
    """acks: [(id, monotonic seconds)] in acknowledgement order; present: ids on the writer."""
    acks = sorted(acks, key=lambda ack: ack[1])
    lost = sorted({ack_id for ack_id, _ in acks} - set(present))
    gap = max(((later[1] - earlier[1], earlier[0], later[0]) for earlier, later in zip(acks, acks[1:])), default=None)
    return dict(acked=len(acks), present=len(present), lost=lost,
                longest_gap_seconds=round(gap[0], 3) if gap else None,
                longest_gap_between=[gap[1], gap[2]] if gap else None)


class Probe:
    def __init__(self, ployz, environment, cwd, log, *, service, user, database, rate, window, stall):
        self.ployz, self.environment, self.cwd = ployz, environment, cwd
        self.service, self.user, self.database = service, user, database
        self.rate, self.window, self.stall = rate, window, stall
        self.log = log.open("a", buffering=1)
        self.stderr = log.with_suffix(".stderr").open("ab")
        self.lock = threading.Lock()
        self.acks = []
        self.in_flight = {}
        self.sessions = 0
        self.stopping = threading.Event()

    def psql(self, *args):
        return [self.ployz, "exec", "-T", self.service, "--", "psql", "-X", "-q", "-At", "-v", "ON_ERROR_STOP=1",
                "-U", self.user, "-d", self.database, *args]

    def query(self, sql, *, deadline):
        while True:
            try:
                result = subprocess.run(self.psql("-c", sql), cwd=self.cwd, env=self.environment,
                                        capture_output=True, text=True, timeout=60, check=False)
                if not result.returncode:
                    return result.stdout
                failure = result.stderr.strip()
            except subprocess.TimeoutExpired:
                failure = "timed out after 60s"
            if time.monotonic() >= deadline:
                raise RuntimeError(f"ackprobe: {sql!r} failed: {failure}")
            time.sleep(1)

    def event(self, **fields):
        with self.lock:
            self.log.write(json.dumps(dict(at=time.time(), t=time.monotonic(), **fields)) + "\n")

    def read(self, session):
        for line in session.stdout:
            line = line.strip()
            if not line.isdigit():
                continue
            ack_id, now = int(line), time.monotonic()
            with self.lock:
                self.in_flight.pop(ack_id, None)
                self.acks.append((ack_id, now))
                self.log.write(json.dumps(dict(at=time.time(), t=now, ack=ack_id)) + "\n")

    def session(self):
        session = subprocess.Popen(self.psql(), cwd=self.cwd, env=self.environment, text=True, bufsize=1,
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.stderr)
        reader = threading.Thread(target=self.read, args=(session,), daemon=True)
        reader.start()
        self.sessions += 1
        self.event(session=self.sessions)
        return session, reader

    def end(self, session, reader, reason):
        session.kill()
        session.wait()
        reader.join(5)
        with self.lock:
            dropped = sorted(self.in_flight)
            self.in_flight.clear()
        self.event(session_ended=self.sessions, reason=reason, unacknowledged=dropped)

    def run(self):
        self.query(f"CREATE TABLE IF NOT EXISTS {TABLE} (id bigint PRIMARY KEY, at timestamptz NOT NULL DEFAULT now())",
                   deadline=time.monotonic() + 300)
        next_id = int(self.query(f"SELECT coalesce(max(id), 0) + 1 FROM {TABLE}", deadline=time.monotonic() + 60))
        session, reader = self.session()
        interval = 1 / self.rate
        due = time.monotonic()
        while not self.stopping.is_set():
            now = time.monotonic()
            with self.lock:
                oldest = min(self.in_flight.values(), default=now)
                busy = len(self.in_flight) >= self.window
            if session.poll() is not None or now - oldest > self.stall:
                self.end(session, reader, "exited" if session.returncode is not None else "stalled")
                time.sleep(0.25)
                session, reader = self.session()
                due = time.monotonic()
                continue
            if busy or now < due:
                time.sleep(min(interval, max(due - now, 0.001)))
                continue
            with self.lock:
                self.in_flight[next_id] = now
            try:
                session.stdin.write(f"INSERT INTO {TABLE} (id) VALUES ({next_id}) RETURNING id;\n")
                session.stdin.flush()
            except (BrokenPipeError, OSError):
                pass
            next_id += 1
            due = max(due + interval, now - 1)
        deadline = time.monotonic() + 5
        while self.in_flight and session.poll() is None and time.monotonic() < deadline:
            time.sleep(0.05)
        self.end(session, reader, "stopped")
        present = [int(line) for line in self.query(f"SELECT id FROM {TABLE}", deadline=time.monotonic() + 300).split()]
        with self.lock:
            result = report(self.acks, present)
        result.update(sent=next_id - 1, sessions=self.sessions)
        self.event(report=result)
        return result


def run(ployz, environment, cwd, evidence, *, service, user="postgres", database="postgres", rate=200, window=16, stall=2.0):
    """Run until SIGINT or SIGTERM, then return the report; the acknowledgement log stays in evidence."""
    log = Path(evidence) / f"ackprobe-{service}-{time.strftime('%Y%m%dT%H%M%S')}.jsonl"
    probe = Probe(ployz, environment, cwd, log, service=service, user=user, database=database,
                  rate=rate, window=window, stall=stall)
    for number in [signal.SIGINT, signal.SIGTERM]:
        signal.signal(number, lambda *_: probe.stopping.set())
    result = probe.run()
    result["log"] = str(log)
    return result
