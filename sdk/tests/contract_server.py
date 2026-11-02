"""Run one SDK's contract client against a disposable, strict loopback API.

Usage: python3 sdk/tests/contract_server.py <client command> [arguments...]
No Docker, real daemon, or user workloads are accessed.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from threading import Thread


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, data):
        raw = json.dumps(data).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        try:
            self.wfile.write(raw)
        except (BrokenPipeError, ConnectionResetError):
            pass

    def do_GET(self):
        if self.path == "/api/session":
            self.server.token += 1
            return self.reply(200, {"token": str(self.server.token)})
        if self.path == "/api/health":
            return self.reply(200, {"app": "init", "version": "0.5.0"})
        if self.path == "/api/state":
            return self.reply(200, {"services": [{"id": "worker", "status": "running"}]})
        if self.path == "/api/services/worker/logs?tail=12":
            return self.reply(200, {"logs": "hello\n"})
        if self.path == "/api/allocation":
            return self.reply(200, {"services": []})
        self.reply(404, {"error": "Unknown endpoint"})

    def do_POST(self):
        self.server.posts.append(self.path)
        if self.headers.get("X-Init-Token") != str(self.server.token):
            return self.reply(403, {"error": "Missing or stale token"})
        self.server.token += 1  # A new token is required for the next mutation.
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
        if self.path == "/api/services/blocked/start":
            return self.reply(409, {"error": "Service is disabled"})
        if self.path == "/api/services/expired/start":
            return self.reply(403, {"error": "Session expired"})
        if self.path == "/api/services/slow/start":
            time.sleep(0.3)
            return self.reply(200, {"ok": True})
        if self.path == "/api/services":
            assert body["service"]["id"] == "worker", body
            assert isinstance(body["apply"], bool), body
            if "github" in body:
                assert body["github"] == {"repository": "https://github.com/example/app", "reference": "main", "context": "services/web"}, body
            return self.reply(200, {"applied": body["apply"], "github": body.get("github")})
        expected = {
            "/api/services/worker/start": {},
            "/api/services/worker/stop": {},
            "/api/services/worker/restart": {},
            "/api/services/worker/run": {},
            "/api/services/worker/apply": {"limits": {"cpus": 1, "memory_mb": 256}},
            "/api/projects": {"id": "dev", "name": "Development"},
            "/api/budget": {"cpus": 1, "memory_mb": 256},
        }
        if self.path == "/api/allocation":
            assert body == {"enabled": True, "apply": body["apply"]}, body
            return self.reply(200, {"applied": body["apply"]})
        assert self.path in expected and body == expected[self.path], (self.path, body)
        self.reply(200, {"ok": True})


if __name__ == "__main__":
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.token = 0
    server.posts = []
    Thread(target=server.serve_forever, daemon=True).start()
    env = {**os.environ, "INIT_TEST_PORT": str(server.server_port),
           "PYTHONPATH": str(Path(__file__).resolve().parents[1] / "python")}
    try:
        result = subprocess.run(sys.argv[1:], env=env)
        if result.returncode:
            sys.exit(result.returncode)
        assert len(server.posts) == 16, server.posts
        assert server.posts.count("/api/services/expired/start") == 1, "Mutation was retried"
        assert server.posts.count("/api/services/blocked/start") == 1, "Mutation was retried"
        assert server.posts.count("/api/services/slow/start") == 1, "Timed-out mutation was retried"
        print("SDK HTTP contract passed; failed mutations were not replayed.")
    finally:
        server.shutdown()
        server.server_close()
