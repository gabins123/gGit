#!/usr/bin/env python3
"""Send one command to a running gGit's dev control bridge and print the result.

    gitcomet-ctl.py [--dir DIR] [--timeout SECONDS] <command...>

DIR defaults to $GITCOMET_CONTROL_DIR. See docs/control-bridge.md.
"""
import argparse
import itertools
import os
import sys
import time

_counter = itertools.count()


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    parser.add_argument("--dir", default=os.environ.get("GITCOMET_CONTROL_DIR"))
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not args.dir:
        parser.error("no control dir: pass --dir or set GITCOMET_CONTROL_DIR")
    if not args.command:
        parser.error("no command")
    os.makedirs(args.dir, exist_ok=True)

    name = "%d-%05d-%d" % (time.time_ns(), next(_counter), os.getpid())
    cmd = os.path.join(args.dir, name + ".cmd")
    result = os.path.join(args.dir, name + ".result")
    # Write under another name and rename, so the app never reads a partial command.
    with open(cmd + ".tmp", "w", newline="\n") as f:
        f.write(" ".join(args.command) + "\n")
    os.replace(cmd + ".tmp", cmd)

    deadline = time.monotonic() + args.timeout
    while time.monotonic() < deadline:
        try:
            with open(result, newline="") as f:
                text = f.read()
        except FileNotFoundError:
            time.sleep(0.05)
            continue
        os.remove(result)
        sys.stdout.write(text)
        return 1 if text.startswith("error:") else 0
    try:
        os.remove(cmd)  # withdraw it if the app never picked it up
    except FileNotFoundError:
        pass
    sys.stderr.write("error: timed out after %gs waiting for %s\n" % (args.timeout, result))
    return 1


if __name__ == "__main__":
    sys.exit(main())
