#!/usr/bin/env python3
"""ackprobe's verdict: which acknowledged ids are missing from the writer, and the longest acknowledgement gap."""

from pathlib import Path
import re
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from ackprobe import Probe, report  # noqa: E402


class Report(unittest.TestCase):
    def test_acked_id_missing_from_the_writer_is_lost(self):
        result = report([(1, 0.0), (2, 0.1), (3, 0.2)], present=[1, 3])
        self.assertEqual(result["lost"], [2])

    def test_unacknowledged_rows_on_the_writer_are_not_lost(self):
        result = report([(1, 0.0), (3, 0.2)], present=[1, 2, 3, 4])
        self.assertEqual((result["lost"], result["acked"], result["present"]), ([], 2, 4))

    def test_longest_gap_names_the_acknowledgements_around_it(self):
        result = report([(1, 0.0), (2, 0.1), (19, 12.6), (20, 12.7)], present=[1, 2, 19, 20])
        self.assertEqual((result["longest_gap_seconds"], result["longest_gap_between"]), (12.5, [2, 19]))

    def test_gap_follows_acknowledgement_time_not_id_order(self):
        result = report([(6, 1.0), (4, 0.0), (5, 4.0)], present=[4, 5, 6])
        self.assertEqual(result["longest_gap_between"], [6, 5])

    def test_no_acknowledgements_has_no_gap(self):
        result = report([], present=[])
        self.assertEqual((result["lost"], result["longest_gap_seconds"]), ([], None))


class ProbeRun(unittest.TestCase):
    def test_sent_counts_only_inserts_from_this_invocation(self):
        with tempfile.TemporaryDirectory() as directory:
            probe = Probe("ployz", {}, directory, Path(directory) / "acks.jsonl", service="db",
                          user="postgres", database="postgres", rate=200, window=16, stall=2)
            self.addCleanup(probe.log.close)
            self.addCleanup(probe.stderr.close)
            session, reader = Mock(), Mock()
            session.poll.return_value = None
            inserted = []

            def insert(sql):
                ack_id = int(re.search(r"VALUES \((\d+)\)", sql)[1])
                inserted.append(ack_id)
                probe.in_flight.pop(ack_id)
                probe.acks.append((ack_id, 1.0))
                probe.stopping.set()

            session.stdin.write.side_effect = insert
            probe.ready.set()
            with patch.object(probe, "query", side_effect=["", "100", "100"]), \
                 patch.object(probe, "session", return_value=(session, reader)):
                result = probe.run()
            self.assertEqual(inserted, [100])
            self.assertEqual((result["sent"], result["acked"], result["lost"]), (1, 1, []))

    def test_a_slow_connect_is_not_a_stall(self):
        with tempfile.TemporaryDirectory() as directory:
            probe = Probe("ployz", {}, directory, Path(directory) / "acks.jsonl", service="db",
                          user="postgres", database="postgres", rate=200, window=16, stall=2)
            self.addCleanup(probe.log.close)
            self.addCleanup(probe.stderr.close)
            clock = [0.0]
            session, reader = Mock(), Mock()
            session.poll.return_value = None
            sessions = []

            def sleep(seconds):
                clock[0] += seconds
                if clock[0] >= 2.5:
                    probe.ready.set()

            def insert(sql):
                # Cloud's dial takes 2.5 s; an insert written before it lands is answered only after it.
                if clock[0] >= 2.5:
                    probe.in_flight.pop(int(re.search(r"VALUES \((\d+)\)", sql)[1]))
                    probe.acks.append((100, clock[0]))
                    probe.stopping.set()

            def open_session():
                sessions.append(clock[0])
                probe.ready.clear()
                return session, reader

            session.stdin.write.side_effect = insert
            with patch.object(probe, "query", side_effect=["", "100", "100"]), \
                 patch.object(probe, "session", side_effect=open_session), \
                 patch("ackprobe.time.monotonic", side_effect=lambda: clock[0]), \
                 patch("ackprobe.time.sleep", side_effect=sleep), \
                 patch("ackprobe.time.time", return_value=0.0):
                result = probe.run()
            self.assertEqual((len(sessions), result["acked"], result["lost"]), (1, 1, []))

    def test_a_session_that_exits_before_ready_is_replaced_not_fatal(self):
        with tempfile.TemporaryDirectory() as directory:
            probe = Probe("ployz", {}, directory, Path(directory) / "acks.jsonl", service="db",
                          user="postgres", database="postgres", rate=200, window=16, stall=2)
            self.addCleanup(probe.log.close)
            self.addCleanup(probe.stderr.close)
            gone = Mock()
            gone.stdin.write.side_effect = BrokenPipeError
            gone.stdout = iter([])
            with patch("ackprobe.subprocess.Popen", return_value=gone):
                session, reader = probe.session()
            reader.join(1)
            self.assertIs(session, gone)


if __name__ == "__main__":
    unittest.main()
