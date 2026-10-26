#!/usr/bin/env python3
"""Keep a native editor's stdio open through private FIFOs and record real RPC replies.

Create the two FIFOs first, start this bridge, then launch the owned app through
the native UI harness with its stdin/stdout redirected to them. Send identified
JSON-RPC requests on this program's stdin. Keep the bridge open until the editor
has naturally exited; {"bridge":"stop"} closes it. This program never launches
an app or proves process exit. Records contain document data: keep them private.
"""
import argparse
import datetime
import json
import os
from pathlib import Path
import select
import stat
import sys
import time


class Lines:
    def __init__(self, fd):
        self.fd = fd
        self.pending = b""

    def read(self, deadline):
        while b"\n" not in self.pending:
            left = deadline - time.monotonic()
            if left <= 0 or not select.select([self.fd], [], [], left)[0]:
                raise TimeoutError("no complete editor reply before deadline")
            chunk = os.read(self.fd, 65536)
            if not chunk:
                raise EOFError("editor output closed before a complete reply")
            self.pending += chunk
            if len(self.pending) > 16 * 1024 * 1024:
                raise ValueError("editor reply exceeds 16 MiB")
        line, self.pending = self.pending.split(b"\n", 1)
        return line


def write_all(fd, data, deadline):
    while data:
        left = deadline - time.monotonic()
        if left <= 0 or not select.select([], [fd], [], left)[1]:
            raise TimeoutError("editor input did not accept the request")
        written = os.write(fd, data)
        if not written:
            raise BrokenPipeError("editor input write stopped")
        data = data[written:]


def reply_for(line, request):
    reply = json.loads(line)
    if not isinstance(reply, dict) or reply.get("jsonrpc") != "2.0" or reply.get("id") != request["id"]:
        raise ValueError("editor reply does not match the identified request")
    if ("result" in reply) == ("error" in reply):
        raise ValueError("editor reply needs exactly one result or error")
    return reply


def private_fifo(path):
    before = path.lstat()
    if not stat.S_ISFIFO(before.st_mode) or before.st_uid != os.getuid():
        raise ValueError("bridge requires an owned FIFO")
    fd = os.open(path, os.O_RDWR | os.O_NONBLOCK | os.O_NOFOLLOW)
    after = os.fstat(fd)
    if (before.st_dev, before.st_ino) != (after.st_dev, after.st_ino):
        os.close(fd)
        raise ValueError("FIFO changed while opening")
    return fd


def self_test():
    read, write = os.pipe()
    os.set_blocking(read, False)
    try:
        request = {"id": "quit"}
        reply = {"jsonrpc": "2.0", "id": "quit", "result": {"isError": False}}
        data = json.dumps(reply).encode() + b"\n"
        write_all(write, data[:7], time.monotonic() + 1)
        write_all(write, data[7:] + data, time.monotonic() + 1)
        lines = Lines(read)
        assert reply_for(lines.read(time.monotonic() + 1), request) == reply
        assert reply_for(lines.read(time.monotonic() + 1), request) == reply
        try:
            lines.read(time.monotonic())
            raise AssertionError("empty stream must time out")
        except TimeoutError:
            pass
        try:
            reply_for(data, {"id": "other"})
            raise AssertionError("wrong reply id must refuse")
        except ValueError:
            pass
    finally:
        os.close(read)
        os.close(write)
    print("PASS: actual pipe framing, buffered replies, timeout and identity checks")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stdin-pipe", type=Path)
    parser.add_argument("--stdout-pipe", type=Path)
    parser.add_argument("--record", type=Path)
    parser.add_argument("--timeout", type=float, default=10)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if not all((args.stdin_pipe, args.stdout_pipe, args.record)) or not 0 < args.timeout <= 60:
        parser.error("two FIFO paths, a new record path and a timeout in (0,60] are required")
    input_fd = private_fifo(args.stdin_pipe)
    try:
        output_fd = private_fifo(args.stdout_pipe)
        try:
            record_fd = os.open(args.record, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            with os.fdopen(record_fd, "w", encoding="utf-8") as record:
                def emit(event, **values):
                    entry = {"event": event, "time": datetime.datetime.now(datetime.timezone.utc).isoformat(), **values}
                    text = json.dumps(entry, ensure_ascii=False)
                    record.write(text + "\n")
                    record.flush()
                    print(text, flush=True)

                lines = Lines(output_fd)
                emit("ready", pid=os.getpid())
                for command in sys.stdin:
                    request = json.loads(command)
                    if request == {"bridge": "stop"}:
                        break
                    if not isinstance(request, dict) or request.get("jsonrpc") != "2.0" or request.get("id") is None:
                        raise ValueError("bridge sends only identified JSON-RPC requests")
                    emit("request", request=request)
                    try:
                        deadline = time.monotonic() + args.timeout
                        write_all(input_fd, json.dumps(request).encode() + b"\n", deadline)
                        reply = reply_for(lines.read(deadline), request)
                        emit("response", response=reply)
                    except Exception as error:
                        emit("failure", error=str(error))
                        raise
                emit("stopped")
        finally:
            os.close(output_fd)
    finally:
        os.close(input_fd)


if __name__ == "__main__":
    main()
