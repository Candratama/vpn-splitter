#!/usr/bin/env python3
"""Read-only process-tree RSS, charged physical footprint and CPU measurement.

Usage: python3 scripts/measure-macos.py PID --seconds 300
Uses macOS proc_pid_rusage v0. Footprint is the OS charged-memory metric,
not an assertion that every charged page is private anonymous memory.
"""

import argparse
import ctypes
import json
import subprocess
import sys
import time


class Usage(ctypes.Structure):
    _fields_ = [("uuid", ctypes.c_uint8 * 16)] + [
        (name, ctypes.c_uint64)
        for name in (
            "user_ns", "system_ns", "idle_wakeups", "interrupt_wakeups",
            "pageins", "wired", "rss", "footprint", "start", "exit",
        )
    ]


def descendants(root):
    rows = subprocess.check_output(["ps", "-axo", "pid=,ppid="], text=True)
    parents = [tuple(map(int, row.split())) for row in rows.splitlines()]
    selected = {root}
    while True:
        expanded = selected | {pid for pid, parent in parents if parent in selected}
        if expanded == selected:
            return selected
        selected = expanded


def sample(root, libproc):
    result = {}
    for pid in descendants(root):
        usage = Usage()
        if libproc.proc_pid_rusage(pid, 0, ctypes.byref(usage)) != 0:
            if pid == root:
                raise ProcessLookupError(f"Cannot read root PID {root}; app may have exited")
            continue
        result[pid] = {
            "cpu_ns": usage.user_ns + usage.system_ns,
            "rss_bytes": usage.rss,
            "footprint_bytes": usage.footprint,
        }
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pid", type=int)
    parser.add_argument("--seconds", type=float, default=300)
    args = parser.parse_args()
    if sys.platform != "darwin" or args.pid <= 0 or args.seconds <= 0:
        parser.error("Requires macOS, a positive PID, and a positive duration")
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    libproc.proc_pid_rusage.argtypes = [ctypes.c_int, ctypes.c_int, ctypes.c_void_p]
    libproc.proc_pid_rusage.restype = ctypes.c_int
    previous = sample(args.pid, libproc)
    started = time.monotonic()
    prior_time = started
    cpu_ns = 0
    peak_rss = sum(p["rss_bytes"] for p in previous.values())
    peak_footprint = sum(p["footprint_bytes"] for p in previous.values())
    process_set = set(previous)
    while time.monotonic() - started < args.seconds:
        time.sleep(max(0, min(5, args.seconds - (time.monotonic() - started))))
        current = sample(args.pid, libproc)
        now = time.monotonic()
        cpu_ns += sum(
            max(0, values["cpu_ns"] - previous.get(pid, {"cpu_ns": 0})["cpu_ns"])
            for pid, values in current.items()
        )
        process_set.update(current)
        peak_rss = max(peak_rss, sum(p["rss_bytes"] for p in current.values()))
        peak_footprint = max(peak_footprint, sum(p["footprint_bytes"] for p in current.values()))
        previous, prior_time = current, now
    elapsed = prior_time - started
    print(json.dumps({
        "seconds": round(elapsed, 3),
        "cpu_percent_one_core": round(cpu_ns / (elapsed * 1e9) * 100, 4),
        "peak_tree_rss_bytes": peak_rss,
        "peak_tree_physical_footprint_bytes": peak_footprint,
        "pids_observed": sorted(process_set),
        "sampling_seconds": 5,
    }, indent=2))


if __name__ == "__main__":
    main()
