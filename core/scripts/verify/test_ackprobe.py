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
            with patch.object(probe, "query", side_effect=["", "100", "100"]), \
                 patch.object(probe, "session", return_value=(session, reader)):
                result = probe.run()
            self.assertEqual(inserted, [100])
            self.assertEqual((result["sent"], result["acked"], result["lost"]), (1, 1, []))


if __name__ == "__main__":
    unittest.main()
