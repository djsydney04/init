#!/usr/bin/env python3
"""Smoke-test real TUI input and persistence in an isolated POSIX terminal.

Usage: python3 scripts/test-tui.py target/debug/init
Engine commands fail and SSH is replaced by a recording rejection stub. This
does not exercise Docker operations or successful remote SSH connections.
"""

import codecs
import errno
import fcntl
import json
import os
from pathlib import Path
import pty
import select
import signal
import struct
import subprocess
import sys
import tempfile
import termios
import time


class Screen:
    """Extract text from ratatui's cursor/erase output, ignoring presentation."""

    def __init__(self, width=120, height=35):
        self.width, self.height = width, height
        self.rows = [[" "] * width for _ in range(height)]
        self.x = self.y = 0
        self.pending = ""
        self.decoder = codecs.getincrementaldecoder("utf-8")("replace")

    def feed(self, data):
        text = self.pending + self.decoder.decode(data)
        self.pending = ""
        index = 0
        while index < len(text):
            char = text[index]
            if char == "\x1b":
                if index + 1 == len(text):
                    break
                if text[index + 1] == "[":
                    end = index + 2
                    while end < len(text) and not "@" <= text[end] <= "~":
                        end += 1
                    if end == len(text):
                        break
                    self.csi(text[index + 2 : end], text[end])
                    index = end + 1
                    continue
                # Crossterm also switches keypad mode with two-byte escapes.
                index += 2
                continue
            if char == "\r":
                self.x = 0
            elif char == "\n":
                self.y = min(self.height - 1, self.y + 1)
            elif char == "\b":
                self.x = max(0, self.x - 1)
            elif char >= " " and char != "\x7f":
                if self.x < self.width:
                    self.rows[self.y][self.x] = char
                self.x = min(self.width, self.x + 1)
            index += 1
        self.pending = text[index:]

    def csi(self, parameters, command):
        # Ignore colours, private modes, cursor visibility and terminal queries.
        if parameters.startswith(("?", ">", "<")):
            return
        values = [int(value) if value.isdigit() else 0 for value in parameters.split(";")]
        count = values[0] or 1
        if command in ("H", "f"):
            self.y = min(self.height - 1, max(0, count - 1))
            self.x = min(self.width - 1, max(0, (values[1] if len(values) > 1 else 1) - 1))
        elif command == "A":
            self.y = max(0, self.y - count)
        elif command == "B":
            self.y = min(self.height - 1, self.y + count)
        elif command == "C":
            self.x = min(self.width - 1, self.x + count)
        elif command == "D":
            self.x = max(0, self.x - count)
        elif command == "G":
            self.x = min(self.width - 1, max(0, count - 1))
        elif command == "d":
            self.y = min(self.height - 1, max(0, count - 1))
        elif command == "J" and values[0] in (2, 3):
            self.rows = [[" "] * self.width for _ in range(self.height)]
        elif command == "K":
            mode = values[0]
            start = 0 if mode in (1, 2) else self.x
            end = self.width if mode in (0, 2) else self.x + 1
            self.rows[self.y][start:end] = [" "] * (end - start)

    @property
    def text(self):
        return "\n".join("".join(row).rstrip() for row in self.rows)


class Terminal:
    def __init__(self, binary, workspace, manifest):
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", 35, 120, 0, 0))
        self.screen = Screen()
        self.transcript = bytearray()

        def controlling_terminal():
            os.setsid()
            fcntl.ioctl(0, termios.TIOCSCTTY, 0)

        environment = dict(os.environ, TERM="xterm-256color", NO_COLOR="1",
                           PATH=str(workspace) + os.pathsep + os.environ.get("PATH", ""))
        try:
            self.process = subprocess.Popen(
                [str(binary), "--workspace", str(workspace), "--config", str(manifest), "tui"],
                stdin=slave,
                stdout=slave,
                stderr=slave,
                cwd=workspace,
                env=environment,
                preexec_fn=controlling_terminal,
                close_fds=True,
            )
        finally:
            os.close(slave)

    def read(self, timeout):
        if not select.select([self.master], [], [], timeout)[0]:
            return
        try:
            data = os.read(self.master, 65536)
        except OSError as error:
            if error.errno == errno.EIO:  # Linux returns EIO after slave closure.
                return
            raise
        self.transcript.extend(data)
        self.screen.feed(data)

    def wait(self, promise, condition, timeout=15):
        deadline = time.monotonic() + timeout
        while True:
            if condition(self.screen.text):
                return
            if self.process.poll() is not None:
                self.read(0)
                if condition(self.screen.text):
                    return
                raise AssertionError(f"TUI exited before {promise} (code {self.process.returncode})")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise AssertionError(f"Timed out waiting for {promise}")
            self.read(min(remaining, 0.2))

    def send(self, keys):
        if self.process.poll() is not None:
            raise AssertionError("TUI quit while a dialog should still be open")
        os.write(self.master, keys.encode())

    def close(self):
        if self.process.poll() is None:
            os.killpg(self.process.pid, signal.SIGTERM)
            try:
                self.process.wait(timeout=2)
            except subprocess.TimeoutExpired:
                os.killpg(self.process.pid, signal.SIGKILL)
                self.process.wait(timeout=2)
        os.close(self.master)


def fixture(workspace):
    engine = workspace / "unavailable-engine"
    engine.write_text("#!/bin/sh\nexit 1\n")
    engine.chmod(0o700)
    ssh = workspace / "ssh"
    ssh.write_text('#!/bin/sh\nprintf invoked > "$0.invoked"\nexit 1\n')
    ssh.chmod(0o700)
    manifest = workspace / "services.json"
    manifest.write_text(json.dumps({
        "version": 1,
        "engine": {"command": [str(engine)]},
        "budget": {"cpus": 2, "memory_mb": 1024, "max_parallel_jobs": 1},
        "projects": [
            {"id": "build", "name": "Build tools", "description": "Build group"},
            {"id": "empty", "name": "Empty group", "description": "No deployments"},
        ],
        "services": [
            {"id": "builder", "project": "build", "name": "Builder service",
             "description": "Build fixture", "runtime": "docker", "kind": "daemon",
             "enabled": False, "observe_only": True, "container": "tui-test-builder"},
            {"id": "loose", "name": "Loose service", "description": "Ungrouped fixture",
             "runtime": "docker", "kind": "daemon", "enabled": False,
             "observe_only": True, "container": "tui-test-loose"},
        ],
    }))
    (workspace / "machines.json").write_text(json.dumps([{
        "profile": {"id": "untested", "name": "Untested fixture", "host": "127.0.0.1",
                    "ssh_port": None, "init_port": 8787},
        "identity": None,
        "revision": "pty-fixture-revision",
    }]))
    return manifest


def check(binary):
    with tempfile.TemporaryDirectory(prefix="init-tui-smoke-") as directory:
        workspace = Path(directory)
        manifest = fixture(workspace)
        terminal = Terminal(binary, workspace, manifest)
        try:
            terminal.wait("initial service inventory", lambda text:
                          "Ready" in text and "All workspaces" in text
                          and "Builder service" in text and "Loose service" in text)

            terminal.send("w")
            terminal.wait("workspace picker with empty group", lambda text:
                          "Choose workspace" in text and "Empty group" in text)
            terminal.send("jjj\r")
            terminal.wait("empty workspace scope", lambda text:
                          "Services · Empty group" in text and "No services in Empty group." in text
                          and "Builder service" not in text and "Loose service" not in text)
            terminal.send("w")
            terminal.wait("workspace picker reopening", lambda text: "Choose workspace" in text)
            terminal.send("k\r")
            terminal.wait("named workspace isolates services", lambda text:
                          "Services · Build tools" in text and "Builder service" in text
                          and "Loose service" not in text and "Choose workspace" not in text)

            terminal.send("/")
            terminal.wait("search input", lambda text: "Search services" in text)
            terminal.send("q")
            terminal.wait("q is search text, not quit", lambda text:
                          "Search services" in text and "/q" in text)
            terminal.send("\x1b")
            terminal.wait("cancel search restores workspace rows", lambda text:
                          "Search services" not in text and "Builder service" in text
                          and "/q" not in text)

            terminal.send("n")
            terminal.wait("new workspace form", lambda text:
                          "New workspace" in text and "Create on This computer" in text)
            terminal.send("discard-me\x1b")
            terminal.wait("cancel new workspace", lambda text: "New workspace" not in text)
            assert not any(project["id"] == "discard-me"
                           for project in json.loads(manifest.read_text())["projects"]), \
                "Canceling a new workspace must not persist it"

            terminal.send("n")
            terminal.wait("new workspace form again", lambda text: "New workspace" in text)
            terminal.send("created-by-tui\tQueue workspace\tCreated through the real PTY\r")
            terminal.wait("successful workspace creation", lambda text:
                          "Workspace created" in text and "New workspace" not in text)
            projects = json.loads(manifest.read_text())["projects"]
            assert projects == [
                {"id": "build", "name": "Build tools", "description": "Build group"},
                {"id": "empty", "name": "Empty group", "description": "No deployments"},
                {"id": "created-by-tui", "name": "Queue workspace",
                 "description": "Created through the real PTY"},
            ], f"New workspace fields must be persisted once without changing existing groups: {projects!r}"
            terminal.wait("created workspace becomes selected", lambda text:
                          "Services · Queue workspace" in text and "No services in Queue workspace." in text)

            terminal.send("m")
            terminal.wait("saved machine picker", lambda text:
                          "Choose machine" in text and "Untested fixture" in text and "test first" in text)
            terminal.send("j\r")
            terminal.wait("untested machine rejected", lambda text:
                          "Test this connection" in text and "Choose machine" in text)
            assert not (workspace / "ssh.invoked").exists(), \
                "An untested machine must be rejected before invoking SSH"
            terminal.send("\x1b")
            terminal.wait("machine rejection preserves local target and scope", lambda text:
                          "Choose machine" not in text and "This computer" in text
                          and "Services · Queue workspace" in text)

            terminal.send("h")
            terminal.wait("host activity view", lambda text: "Host activity" in text)
            terminal.send("?")
            terminal.wait("host help", lambda text:
                          "Init help" in text and "HOST ACTIVITY" in text)
            terminal.send("\x1b")
            terminal.wait("help closes to host", lambda text:
                          "Init help" not in text and "Host activity" in text)
            terminal.send("\x1b")
            terminal.wait("host closes to service scope", lambda text:
                          "Host activity" not in text and "Services · Queue workspace" in text)

            terminal.send("q")
            terminal.wait("clean exit", lambda _text: terminal.process.poll() is not None)
            terminal.read(0)
            assert terminal.process.returncode == 0, f"Quit exited with {terminal.process.returncode}"
            assert b"\x1b[?1049l" in terminal.transcript, "Quit must restore the primary terminal screen"
            assert b"\x1b[?25h" in terminal.transcript, "Quit must restore the cursor"
            assert not (workspace / "ssh.invoked").exists(), "Local journeys must never invoke SSH"
        except BaseException:
            print("TUI screen at failure:\n" + terminal.screen.text, file=sys.stderr)
            raise
        finally:
            terminal.close()


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python3 scripts/test-tui.py PATH_TO_INIT")
    executable = Path(sys.argv[1]).resolve(strict=True)
    check(executable)
    print("TUI PTY smoke passed: scoped inventory, search/cancel, persisted workspace, machine guard, host/help, clean quit")
