#!/usr/bin/env python3
"""Exercise the warm move script's mirror cron wait without a cluster."""

import importlib.util
from pathlib import Path
import unittest


spec = importlib.util.spec_from_file_location("warm_move", Path(__file__).resolve().parents[1] / "test-verify-warm-move.py")
warm_move = importlib.util.module_from_spec(spec)
spec.loader.exec_module(warm_move)

CRON, RUNS = warm_move.MIRROR_CRON, warm_move.MIRROR_CRON_RUNS


def resumes(into, span):
    return (into + warm_move.mirror_cron_wait(into, span)) % CRON


class MirrorCronWait(unittest.TestCase):
    def test_a_start_clear_of_the_cron_goes_at_once(self):
        for into in (RUNS + 0.01, 400, CRON - 30 - 0.01):
            with self.subTest(into=into):
                self.assertEqual(warm_move.mirror_cron_wait(into, 30), 0)

    def test_a_start_inside_the_cron_resumes_after_its_run(self):
        for into in (0, 0.5, 45, RUNS - 0.4, RUNS - 0.01):
            with self.subTest(into=into):
                self.assertGreaterEqual(resumes(into, 30), RUNS)
                self.assertLessEqual(resumes(into, 30), RUNS + 1)

    def test_a_span_reaching_the_next_cron_resumes_after_that_run(self):
        for into, span in ((CRON - 30 + 0.4, 30), (CRON - 0.4, 30), (CRON - 300, 300), (700, 300)):
            with self.subTest(into=into, span=span):
                self.assertGreater(warm_move.mirror_cron_wait(into, span), 0)
                self.assertGreaterEqual(resumes(into, span), RUNS)
                self.assertLessEqual(resumes(into, span) + span, CRON)


if __name__ == "__main__":
    unittest.main()
