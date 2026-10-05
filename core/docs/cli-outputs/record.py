#!/usr/bin/env python3
"""Run a shell command in a PTY and record what a terminal shows."""
import argparse
import fcntl
import json
import os
import pty
import select
import shlex
import struct
import subprocess
import sys
import tempfile
import termios
import time

HOLD_LAST_FRAME = 2.5


def record(cmd, cols, rows):
    pid, fd = pty.fork()
    if pid == 0:
        os.environ.update(TERM="xterm-256color", COLUMNS=str(cols), LINES=str(rows))
        os.execvp("bash", ["bash", "-c", cmd])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))
    start = time.monotonic()
    events = []
    while True:
        ready, _, _ = select.select([fd], [], [], 0.05)
        if not ready:
            continue
        try:
            data = os.read(fd, 65536)
        except OSError:
            break
        if not data:
            break
        events.append([round(time.monotonic() - start, 4), "o", data.decode("utf-8", "replace")])
    _, status = os.waitpid(pid, 0)
    events.append([round(time.monotonic() - start + HOLD_LAST_FRAME, 4), "o", ""])
    return events, os.waitstatus_to_exitcode(status)


def write_cast(path, events, cols, rows):
    with open(path, "w") as f:
        f.write(json.dumps({"version": 2, "width": cols, "height": rows}) + "\n")
        for event in events:
            f.write(json.dumps(event) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--cols", type=int, default=100)
    parser.add_argument("--rows", type=int, default=30)
    parser.add_argument("--cast")
    parser.add_argument("--gif")
    parser.add_argument("--png")
    parser.add_argument("--expect-exit", type=int, default=0, metavar="N")
    parser.add_argument("cmd", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    cmd = args.cmd[1:] if args.cmd[:1] == ["--"] else args.cmd
    if not cmd or not (args.cast or args.gif or args.png):
        parser.error("give a command after -- and at least one of --cast, --gif, --png")

    events, code = record(" ".join(cmd) if len(cmd) == 1 else shlex.join(cmd), args.cols, args.rows)
    if code != args.expect_exit:
        sys.exit(f"record: the command exited {code}, not {args.expect_exit}; nothing written")
    # Render everything into a temp dir, then place each file, so a
    # failed render leaves the existing outputs as they were.
    with tempfile.TemporaryDirectory() as tmp:
        cast = os.path.join(tmp, "rec.cast")
        gif = os.path.join(tmp, "rec.gif")
        png = os.path.join(tmp, "rec.png")
        write_cast(cast, events, args.cols, args.rows)
        if args.gif or args.png:
            subprocess.run(["agg", "--quiet", cast, gif], check=True)
        if args.png:
            subprocess.run(
                ["ffmpeg", "-loglevel", "error", "-y", "-i", gif, "-update", "1", png],
                check=True,
            )
        for rendered, dest in ((cast, args.cast), (gif, args.gif), (png, args.png)):
            if dest:
                place(rendered, dest)


def place(src, dest):
    """Copy src to a temp file beside dest, then rename it over dest."""
    fd, landing = tempfile.mkstemp(dir=os.path.dirname(os.path.abspath(dest)), prefix=".record.")
    try:
        with os.fdopen(fd, "wb") as out, open(src, "rb") as data:
            out.write(data.read())
        os.chmod(landing, 0o644)
        os.replace(landing, dest)
    except BaseException:
        os.unlink(landing)
        raise


if __name__ == "__main__":
    main()
