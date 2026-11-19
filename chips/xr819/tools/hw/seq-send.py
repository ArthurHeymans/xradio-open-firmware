#!/usr/bin/env python3
"""Paced UDP sender with an embedded sequence number.

Usage: seq-send.py TARGET PORT MBIT SIZE SECONDS [SOURCE_PORT]

The fixed source port and receiver hello open a conntrack entry through the
workstation firewall. A missing hello fails the run instead of sending before
the receiver is ready. Stdout's SENT report must reach the receiver over the
independent management connection, not over the link under test.
"""

import argparse
import contextlib
import socket
import struct
import sys
import time


def send(args):
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.bind(("", args.source_port))
        sock.connect((args.target, args.port))
        hello_deadline = time.monotonic() + args.hello_timeout
        while True:
            remaining = hello_deadline - time.monotonic()
            if remaining <= 0:
                raise ValueError("receiver hello timed out")
            sock.settimeout(min(0.5, remaining))
            # When the firewalled host sends, an unsolicited receiver hello
            # cannot reach it. Solicit the reply to open conntrack first.
            with contextlib.suppress(OSError):
                sock.send(b"HELLO?")
            try:
                if sock.recv(64) == b"HELLO":
                    break
            except OSError:
                # An early hello to a not-yet-bound source port can leave an
                # ICMP error queued on the connected UDP socket. Keep waiting.
                continue
        sock.settimeout(None)
        pad = b"\0" * (args.size - 8)
        interval = (args.size * 8) / (args.mbit * 1e6)
        sent = errors = 0
        start = time.monotonic()
        start_epoch_ns = time.time_ns()  # Approximate wall-clock origin, not airtime.
        deadline = start
        while time.monotonic() - start < args.seconds:
            payload = struct.pack(
                "!II", sent, int((time.monotonic() - start) * 1e6) & 0xFFFFFFFF,
            ) + pad
            try:
                sock.send(payload)
                sent += 1
            except OSError as error:
                errors += 1
                if errors <= 3:
                    print(f"SEND_ERROR={error}", file=sys.stderr)
            deadline += interval
            delay = deadline - time.monotonic()
            if delay > 0.0002:
                time.sleep(delay)
            elif delay < -0.05:
                deadline = time.monotonic()
        print(f"START_EPOCH_NS={start_epoch_ns}")
        print(f"SENT={sent} ERRORS={errors} SECS={time.monotonic() - start:.6f}")
        if sent == 0:
            raise ValueError("sender transmitted no datagrams")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target")
    parser.add_argument("port", type=int)
    parser.add_argument("mbit", type=float)
    parser.add_argument("size", type=int)
    parser.add_argument("seconds", type=float)
    parser.add_argument("source_port", nargs="?", type=int, default=5051)
    parser.add_argument("--hello-timeout", type=float, default=10.0)
    args = parser.parse_args()
    if not (args.mbit > 0 and args.seconds > 0 and 8 <= args.size <= 65507
            and args.hello_timeout > 0):
        parser.error("positive rate/durations and a UDP payload size in 8..65507 are required")
    try:
        send(args)
    except (OSError, ValueError) as error:
        print(f"FLOW_ERROR {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
