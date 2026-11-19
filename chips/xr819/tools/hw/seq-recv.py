#!/usr/bin/env python3
"""Receive sequence-numbered UDP, using an independent sender report.

Usage: seq-recv.py BIND_ADDR SENDER_ADDR PORT SECONDS [HELLO_PORT]
                   --sent-file PATH [--drain-secs SECONDS]

SECONDS bounds the wait for the sender report, not the traffic window. The
controller copies seq-send.py's stdout to PATH over the management connection,
atomically, after the sender exits. Keep receiving for a drain interval after
that report arrives. Never infer how much was sent from received packets: that
would hide a lost tail, or an entirely lost flow.

The periodic hello opens the workstation firewall's UDP conntrack entry.
A window run is a gap of 60-70 datagrams (see the RX investigation).
The longest gap includes the existing payload send timestamps on either side,
plus approximate wall-clock bounds when the sender report supplies its origin.
These are payload-construction times, not airtime or missing-packet timestamps.
"""

import argparse
import contextlib
import os
import re
import socket
import struct
import sys
import time
from collections import Counter
from pathlib import Path


def sender_report(path):
    """Read the completed report; an absent file means the sender is still busy."""
    try:
        text = Path(path).read_text()
    except FileNotFoundError:
        return None
    reports = re.findall(r"^SENT=(\d+) ERRORS=(\d+) SECS=([0-9.]+)$", text, re.MULTILINE)
    if len(reports) != 1:
        raise ValueError("missing or ambiguous sender report")
    sent, errors, duration = reports[0]
    epochs = re.findall(r"^START_EPOCH_NS=(.*)$", text, re.MULTILINE)
    if len(epochs) > 1:
        raise ValueError("ambiguous sender clock origin")
    epoch = int(epochs[0]) if epochs else None
    # The existing payload timestamp is u32 microseconds. Do not assign
    # absolute bounds after a possible wrap, or guess an origin for old logs.
    duration = float(duration)
    if duration >= (1 << 32) / 1e6:
        epoch = None
    return int(sent), int(errors), epoch, duration


def socket_drops(sock):
    """Linux's per-socket drop count; read once, while the socket is open."""
    try:
        inode = str(os.fstat(sock.fileno()).st_ino)
        for line in Path("/proc/self/net/udp").read_text().splitlines()[1:]:
            fields = line.split()
            # /proc/net/udp combines queue/timer pairs into single fields;
            # inode is field 9 and the final field is the socket drop count.
            if len(fields) > 10 and fields[9] == inode:
                return int(fields[-1])
    except (OSError, ValueError):
        pass
    return None


class GapTiming:
    """Retain timestamps at gap edges and late arrivals, not every packet."""

    def __init__(self):
        self.highest = -1
        self.last_send_us = None
        self.edges = {}

    def observe(self, seq, send_us):
        # Called for a new sequence only; duplicates must not change timing.
        if seq < self.highest:
            self.edges[seq] = send_us
            return True
        if seq > self.highest + 1:
            if self.highest >= 0:
                self.edges[self.highest] = self.last_send_us
            self.edges[seq] = send_us
        self.highest, self.last_send_us = seq, send_us
        return False

    def at(self, seq):
        return self.last_send_us if seq == self.highest else self.edges.get(seq)


def summarize(received, offered, reordered=0, duplicates=0, send_errors=0,
              timing=None, sender_epoch_ns=None, buffer_bytes=None, drops=None,
              sender_duration_s=None):
    if offered <= 0:
        raise ValueError("sender transmitted no datagrams")
    if any(seq < 0 or seq >= offered for seq in received):
        raise ValueError("received sequence outside the sender's reported range")
    runs = []
    previous = -1
    # Include the *sent* tail even when none of it arrived. Avoid materializing
    # every missing sequence in a badly degraded or completely lost flow.
    for seq in sorted(received):
        if seq > previous + 1:
            runs.append((previous + 1, seq - 1))
        previous = seq
    if previous + 1 < offered:
        runs.append((previous + 1, offered - 1))
    lengths = Counter(end - begin + 1 for begin, end in runs)
    loss = 100.0 * (offered - len(received)) / offered
    window_runs = sum(count for length, count in lengths.items() if 60 <= length <= 70)
    lines = [
        f"FLOW offered={offered} received={len(received)} loss={loss:.2f}% "
        f"runs={len(runs)} max_run={max(lengths, default=0)} singles={lengths[1]} "
        f"window_runs={window_runs} reordered={reordered} duplicates={duplicates} "
        f"send_errors={send_errors}",
        f"RUN_LENGTHS {sorted(lengths.items(), key=lambda kv: -kv[1])[:8]}",
    ]
    if sender_duration_s is not None:
        lines[0] += f" sender_duration_s={sender_duration_s:.6f}"
    if sender_epoch_ns is not None:
        lines[0] += f" sender_start_epoch_ns={sender_epoch_ns}"
    if buffer_bytes is not None:
        lines[0] += f" receiver_buffer_bytes={buffer_bytes} receiver_socket_drops={drops if drops is not None else 'unknown'}"
    if runs:
        begin, end = max(runs, key=lambda run: run[1] - run[0])
        bounds = ""
        if timing is not None:
            for label, seq in [("before", begin - 1), ("after", end + 1)]:
                stamp = timing.at(seq)
                bounds += f" {label}_send_us={stamp if stamp is not None else 'unknown'}"
                if sender_epoch_ns is not None and stamp is not None:
                    bounds += f" {label}_epoch_ns={sender_epoch_ns + 1000 * stamp}"
        lines.append(f"LONGEST_RUN first={begin} last={end} of {offered}{bounds}")
    return "\n".join(lines)


def receive(args):
    peer = (socket.gethostbyname(args.sender_addr), args.hello_port)
    received = set()
    reordered = duplicates = 0
    timing = GapTiming()
    report = None
    drain_deadline = None
    deadline = time.monotonic() + args.seconds
    next_report_check = 0.0
    last_hello = -float("inf")
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        # Absorb short scheduler pauses. The kernel may clamp this request;
        # report the actual size and reject known local socket overflow.
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 1024 * 1024)
        buffer_bytes = sock.getsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF)
        sock.bind((args.bind_addr, args.port))
        sock.settimeout(0.1)
        while True:
            now = time.monotonic()
            # Do not stat a missing file for every packet on the slow board.
            if report is None and now >= next_report_check:
                next_report_check = now + 0.1
                report = sender_report(args.sent_file)
                if report is not None:
                    drain_deadline = now + args.drain_secs
                elif now >= deadline:
                    raise ValueError("timed out waiting for the independent sender report")
            if drain_deadline is not None and now >= drain_deadline:
                break
            if report is None and now - last_hello >= 0.5:
                with contextlib.suppress(OSError):
                    sock.sendto(b"HELLO", peer)
                last_hello = now
            try:
                data, source = sock.recvfrom(65535)
            except TimeoutError:
                continue
            if source != peer:
                continue
            if data == b"HELLO?":
                with contextlib.suppress(OSError):
                    sock.sendto(b"HELLO", peer)
                continue
            if len(data) < 8:
                continue
            seq, send_us = struct.unpack("!II", data[:8])
            if seq in received:
                duplicates += 1
            else:
                reordered += timing.observe(seq, send_us)
                received.add(seq)
        drops = socket_drops(sock)
    assert report is not None
    summary = summarize(received, report[0], reordered, duplicates, report[1],
                        timing, report[2], buffer_bytes, drops, report[3])
    if drops:
        raise ValueError(f"receiver socket overflowed ({drops} datagrams); measurement invalid; {summary}")
    return summary


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("bind_addr")
    parser.add_argument("sender_addr")
    parser.add_argument("port", type=int)
    parser.add_argument("seconds", type=float)
    parser.add_argument("hello_port", nargs="?", type=int, default=5051)
    parser.add_argument("--sent-file", required=True)
    parser.add_argument("--drain-secs", type=float, default=1.0)
    args = parser.parse_args()
    if args.seconds <= 0 or args.drain_secs < 0:
        parser.error("seconds must be positive and drain-secs nonnegative")
    try:
        print(receive(args))
    except (OSError, ValueError) as error:
        print(f"FLOW_ERROR {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
