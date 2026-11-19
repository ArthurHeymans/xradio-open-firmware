#!/usr/bin/env python3
"""Sequence-flow regressions; no board, Wi-Fi, root or third-party modules."""

import os
import random
import re
import runpy
import socket
import struct
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch

HW = Path(__file__).resolve().parent
RECEIVER = HW / "seq-recv.py"
SENDER = HW / "seq-send.py"
recv = runpy.run_path(str(RECEIVER))


class SummaryTests(unittest.TestCase):
    def test_lost_tail_is_counted(self):
        text = recv["summarize"](set(range(7)), 10)
        self.assertIn("offered=10 received=7 loss=30.00%", text)
        self.assertIn("LONGEST_RUN first=7 last=9 of 10", text)

    def test_complete_loss_is_one_full_length_gap(self):
        text = recv["summarize"](set(), 100)
        self.assertIn("offered=100 received=0 loss=100.00% runs=1 max_run=100", text)
        self.assertIn("LONGEST_RUN first=0 last=99 of 100", text)

    def test_leading_internal_and_tail_gaps(self):
        text = recv["summarize"]({2, 3, 5}, 8)
        self.assertIn("runs=3 max_run=2 singles=1", text)
        self.assertIn("RUN_LENGTHS [(2, 2), (1, 1)]", text)

    def test_reorder_window_gap_includes_the_tail(self):
        self.assertIn("window_runs=1", recv["summarize"]({0}, 65))

    def test_foreign_sequences_and_empty_sender_do_not_produce_valid_flows(self):
        for received, offered in [({5}, 5), ({-1}, 5), (set(), 0)]:
            with self.subTest(received=received, offered=offered), self.assertRaises(ValueError):
                recv["summarize"](received, offered)

    def test_absent_malformed_and_duplicate_sender_reports(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "sender.txt"
            self.assertIsNone(recv["sender_report"](path))
            for text in ["", "SENT=12", "SENT=12 ERRORS=0 SECS=1.0\n" * 2]:
                path.write_text(text)
                with self.subTest(text=text), self.assertRaises(ValueError):
                    recv["sender_report"](path)
            path.write_text("SEND_ERROR=example\nSENT=12 ERRORS=1 SECS=1.0\n")
            self.assertEqual(recv["sender_report"](path), (12, 1, None, 1.0))

    def test_actual_sender_duration_is_preserved(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "sender.txt"
            # A blocked send can extend a nominal twenty-second run.
            path.write_text("SENT=120 ERRORS=0 SECS=24.123456\n")
            self.assertEqual(recv["sender_report"](path), (120, 0, None, 24.123456))

    def test_summary_reports_actual_duration_without_changing_loss(self):
        text = recv["summarize"]({0, 1}, 3, sender_duration_s=24.123456)
        self.assertIn("offered=3 received=2 loss=33.33%", text)
        self.assertIn("sender_duration_s=24.123456", text)

    def test_sender_clock_origin_and_timestamp_wrap(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "sender.txt"
            path.write_text("START_EPOCH_NS=1700000000000000000\nSENT=12 ERRORS=0 SECS=1.0\n")
            self.assertEqual(recv["sender_report"](path), (12, 0, 1700000000000000000, 1.0))
            path.write_text("START_EPOCH_NS=1700000000000000000\nSENT=12 ERRORS=0 SECS=4295.0\n")
            self.assertEqual(recv["sender_report"](path), (12, 0, None, 4295.0))
            for origin in ["START_EPOCH_NS=bad\n", "START_EPOCH_NS=1\nSTART_EPOCH_NS=2\n"]:
                path.write_text(origin + "SENT=12 ERRORS=0 SECS=1.0\n")
                with self.assertRaises(ValueError):
                    recv["sender_report"](path)


class LocalDropsTests(unittest.TestCase):
    @unittest.skipUnless(Path("/proc/self/net/udp").exists(), "Linux UDP socket table")
    def test_a_bound_socket_has_a_readable_drop_counter(self):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            sock.bind(("127.0.0.1", 0))
            self.assertEqual(recv["socket_drops"](sock), 0)

    def test_drop_lookup_uses_our_socket_inode(self):
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
            inode = os.fstat(sock.fileno()).st_ino
            row = f"0: 0100007F:0010 00000000:0000 07 0:0 00:0 0 1000 0 {inode} 2 0 7\n"
            with patch.object(Path, "read_text", return_value="header\n" + row):
                self.assertEqual(recv["socket_drops"](sock), 7)

    def test_unavailable_drop_count_is_not_reported_as_zero(self):
        with (socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock,
              patch.object(Path, "read_text", side_effect=FileNotFoundError)):
            self.assertIsNone(recv["socket_drops"](sock))

    def test_receiver_rejects_its_own_socket_overflow(self):
        with tempfile.TemporaryDirectory() as tmp:
            report = Path(tmp) / "sender.txt"
            report.write_text("SENT=17 ERRORS=0 SECS=0.1\n")
            args = SimpleNamespace(sender_addr="127.0.0.1", hello_port=5051,
                                   bind_addr="127.0.0.1", port=0, seconds=1,
                                   sent_file=report, drain_secs=0)
            with (patch.dict(recv["receive"].__globals__, socket_drops=lambda _: 7),
                  self.assertRaisesRegex(ValueError, "receiver socket overflowed.*measurement invalid")):
                recv["receive"](args)


class GapTimingTests(unittest.TestCase):
    def timing(self, sequences):
        timing = recv["GapTiming"]()
        for seq in sequences:
            timing.observe(seq, seq * 100)
        return timing

    def test_internal_gap_uses_observed_boundaries(self):
        timing = self.timing([0, 1, 7, 8])
        text = recv["summarize"]({0, 1, 7, 8}, 9, timing=timing, sender_epoch_ns=1000000)
        self.assertIn("first=2 last=6", text)
        self.assertIn("before_send_us=100 before_epoch_ns=1100000", text)
        self.assertIn("after_send_us=700 after_epoch_ns=1700000", text)

    def test_late_arrivals_split_or_close_a_gap(self):
        timing = self.timing([0, 5, 1, 3, 4])
        text = recv["summarize"]({0, 1, 3, 4, 5}, 6, timing=timing)
        self.assertIn("first=2 last=2", text)
        self.assertIn("before_send_us=100 after_send_us=300", text)
        timing.observe(2, 200)
        self.assertNotIn("LONGEST_RUN", recv["summarize"](set(range(6)), 6, timing=timing))

    def test_no_invented_timestamps_for_leading_tail_or_complete_loss(self):
        for sequences, offered, expected in [
            ([3, 4], 5, "before_send_us=unknown after_send_us=300"),
            ([0, 1], 5, "before_send_us=100 after_send_us=unknown"),
            ([], 5, "before_send_us=unknown after_send_us=unknown"),
        ]:
            text = recv["summarize"](set(sequences), offered, timing=self.timing(sequences))
            self.assertIn(expected, text)
            self.assertNotIn("_epoch_ns=", text)

    def test_clean_stream_does_not_store_each_packet_timestamp(self):
        timing = self.timing(range(10000))
        self.assertEqual(timing.edges, {})
        self.assertEqual(timing.at(9999), 999900)

    def test_sparse_bounds_match_full_timestamps_after_arbitrary_reordering(self):
        randomizer = random.Random(0)
        for _ in range(100):
            sequences = randomizer.sample(range(50), randomizer.randrange(51))
            timing = self.timing(sequences)
            received = set(sequences)
            for missing in set(range(50)) - received:
                for boundary in [missing - 1, missing + 1]:
                    if boundary in received:
                        self.assertEqual(timing.at(boundary), boundary * 100)


class SocketTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.report = Path(self.tmp.name) / "sender.txt"
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.addCleanup(self.sock.close)
        self.sock.bind(("127.0.0.1", 0))
        self.sock.settimeout(3)

    def start_receiver(self, seconds=2.0, drain=0.15):
        process = subprocess.Popen(
            [sys.executable, str(RECEIVER), "127.0.0.1", "127.0.0.1", "0",
             str(seconds), str(self.sock.getsockname()[1]), "--sent-file", str(self.report),
             "--drain-secs", str(drain)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.addCleanup(self.stop, process)
        hello, self.destination = self.sock.recvfrom(64)
        self.assertEqual(hello, b"HELLO")
        return process

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.kill()
        process.communicate(timeout=3)

    def publish(self, sent, errors=0, epoch=None):
        temporary = self.report.with_suffix(".tmp")
        prefix = f"START_EPOCH_NS={epoch}\n" if epoch is not None else ""
        temporary.write_text(prefix + f"SENT={sent} ERRORS={errors} SECS=0.10\n")
        temporary.replace(self.report)

    def packet(self, seq, sock=None, send_us=0):
        (sock or self.sock).sendto(struct.pack("!II", seq, send_us), self.destination)

    def finish(self, process, success=True):
        stdout, stderr = process.communicate(timeout=4)
        if success:
            self.assertEqual(process.returncode, 0, stderr)
        else:
            self.assertNotEqual(process.returncode, 0)
            self.assertNotRegex(stdout, r"(?m)^FLOW ")
        return stdout, stderr

    def test_duplicate_reordering_and_foreign_source(self):
        process = self.start_receiver()
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as foreign:
            self.packet(500, foreign)
        for seq in [0, 2, 2, 1]:
            self.packet(seq)
        self.publish(3, errors=2)
        text, _ = self.finish(process)
        self.assertIn("received=3 loss=0.00%", text)
        self.assertIn("reordered=1 duplicates=1 send_errors=2", text)

    def test_gap_clock_uses_first_unique_packet_not_duplicates(self):
        process = self.start_receiver()
        self.packet(0, send_us=100)
        self.packet(0, send_us=999)
        self.packet(4, send_us=900)
        self.publish(5, epoch=1700000000000000000)
        text, _ = self.finish(process)
        self.assertIn("duplicates=1", text)
        self.assertIn("sender_start_epoch_ns=1700000000000000000", text)
        self.assertIn("before_send_us=100 before_epoch_ns=1700000000000100000", text)
        self.assertIn("after_send_us=900 after_epoch_ns=1700000000000900000", text)

    def test_report_can_arrive_after_the_last_received_packet(self):
        process = self.start_receiver()
        self.packet(0)
        time.sleep(0.3)
        self.assertIsNone(process.poll())
        self.publish(4)
        text, _ = self.finish(process)
        self.assertIn("offered=4 received=1 loss=75.00%", text)

    def test_delayed_packet_during_drain_is_received(self):
        process = self.start_receiver(drain=0.4)
        self.packet(0)
        self.publish(2)
        time.sleep(0.2)
        self.assertIsNone(process.poll())
        self.packet(1)
        text, _ = self.finish(process)
        self.assertIn("received=2 loss=0.00%", text)

    def test_completely_lost_udp_still_has_a_sender_count(self):
        process = self.start_receiver()
        self.publish(17)
        text, _ = self.finish(process)
        self.assertIn("offered=17 received=0 loss=100.00%", text)

    def test_missing_report_fails_instead_of_guessing(self):
        process = self.start_receiver(seconds=0.3)
        self.packet(0)
        _, stderr = self.finish(process, success=False)
        self.assertIn("timed out waiting for the independent sender report", stderr)

    def test_malformed_report_fails(self):
        process = self.start_receiver()
        self.report.write_text("not a completed sender report")
        _, stderr = self.finish(process, success=False)
        self.assertIn("missing or ambiguous sender report", stderr)

    def test_delayed_sender_waits_for_readiness_and_reports_actual_count(self):
        # The receiver's first hello goes to the temporary bound socket. Start
        # the real sender later, on that same source port; it must wait for the
        # next hello and then get its *entire* requested transmission interval.
        process = self.start_receiver()
        source_port = self.sock.getsockname()[1]
        time.sleep(0.3)
        self.sock.close()
        sent = subprocess.run(
            [sys.executable, str(SENDER), "127.0.0.1", str(self.destination[1]),
             "0.05", "100", "0.2", str(source_port), "--hello-timeout", "1"],
            capture_output=True, text=True, timeout=3,
        )
        self.assertEqual(sent.returncode, 0, sent.stderr)
        match = re.search(r"SENT=(\d+)", sent.stdout)
        assert match is not None, sent.stdout
        count = int(match[1])
        self.assertGreater(count, 0)
        self.report.write_text(sent.stdout)
        text, _ = self.finish(process)
        self.assertIn(f"offered={count} received={count} loss=0.00%", text)
        self.assertRegex(sent.stdout, r"SECS=0\.[2-9]\d")
        duration = re.search(r"SECS=([0-9.]+)", sent.stdout)
        assert duration is not None, sent.stdout
        self.assertIn(f"sender_duration_s={float(duration[1]):.6f}", text)

    def test_receiver_answers_solicited_hello_immediately(self):
        process = self.start_receiver()
        self.sock.sendto(b"HELLO?", self.destination)
        self.sock.settimeout(0.2)
        hello, _ = self.sock.recvfrom(64)
        self.assertEqual(hello, b"HELLO")
        self.publish(1)
        self.finish(process)

    def test_sender_solicits_a_reply_before_transmitting_data(self):
        process = subprocess.Popen(
            [sys.executable, str(SENDER), "127.0.0.1", str(self.sock.getsockname()[1]),
             "0.05", "100", "0.1", "0", "--hello-timeout", "1"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        self.addCleanup(self.stop, process)
        solicitation, source = self.sock.recvfrom(64)
        self.assertEqual(solicitation, b"HELLO?")
        self.sock.sendto(b"HELLO", source)
        stdout, stderr = process.communicate(timeout=3)
        self.assertEqual(process.returncode, 0, stderr)
        self.assertRegex(stdout, r"SENT=[1-9]\d*")

    def test_sender_without_receiver_fails_without_a_success_report(self):
        # A bound silent socket avoids relying on ICMP or firewall behaviour.
        result = subprocess.run(
            [sys.executable, str(SENDER), "127.0.0.1", str(self.sock.getsockname()[1]),
             "0.05", "100", "0.1", "0", "--hello-timeout", "0.15"],
            capture_output=True, text=True, timeout=3,
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("SENT=", result.stdout)
        self.assertIn("receiver hello timed out", result.stderr)


class ControllerTests(unittest.TestCase):
    def test_both_management_report_paths_and_failure_propagation(self):
        # Stand in for SSH/SCP with a local shell/cp; exercise the actual shell
        # helper, including report publication and receiver exit propagation.
        with tempfile.TemporaryDirectory() as tmp:
            environment = dict(os.environ, FLOW_HW=str(HW), FLOW_TMP=tmp)
            script = r'''
set -u
. "$FLOW_HW/sequence-flow.sh"
board() {
  if [ "$1" = 'mktemp -d /run/xr819-flow.XXXXXX' ]; then
    mktemp -d "$FLOW_TMP/remote.XXXXXX"
  elif [ "${BREAK_REPORT:-0}" = 1 ] && [[ "$1" == mv* ]]; then
    return 1
  else
    bash -c "$1"
  fi
}
board_put() { cp "$1" "$2"; }
sequence_flow "$DIRECTION" 127.0.0.1 127.0.0.1 "$PORT" 0.05 100 1
'''
            for direction, break_report in [("up", False), ("down", False), ("down", True)]:
                with self.subTest(direction=direction, break_report=break_report):
                    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as probe:
                        probe.bind(("127.0.0.1", 0))
                        port = probe.getsockname()[1]
                    result = subprocess.run(
                        ["bash", "-c", script], capture_output=True, text=True, timeout=8,
                        env=dict(environment, DIRECTION=direction, PORT=str(port),
                                 BREAK_REPORT=str(int(break_report))),
                    )
                    if break_report:
                        self.assertNotEqual(result.returncode, 0)
                        self.assertNotRegex(result.stdout, r"(?m)^FLOW ")
                    else:
                        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                        self.assertRegex(result.stdout, r"SENT=[1-9]\d*")
                        self.assertIn("loss=0.00%", result.stdout)
                        self.assertRegex(result.stdout, r"(?m)^FLOW ")
                    self.assertEqual(list(Path(tmp).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
