"""Synchronous, dependency-free client for a local Self-host Init daemon."""

import http.client
import json
import math
import re
from typing import Any


class ApiError(Exception):
    """A non-2xx daemon response. Inspect status and message for details."""

    def __init__(self, status: int, message: str):
        self.status = status
        self.message = message
        super().__init__(f"HTTP {status}: {message}")


class Client:
    """Connect to numeric loopback; timeout is in seconds per HTTP request.

    Mutations fetch a fresh session token and are never retried. After a timeout,
    inspect state before retrying: the daemon may still finish the operation.
    """

    def __init__(self, port: int = 8787, timeout: float = 30):
        if type(port) is not int or not 1 <= port <= 65535:
            raise ValueError("port must be an integer between 1 and 65535")
        if not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("timeout must be positive and finite")
        self.port = port
        self.timeout = timeout

    def _request(self, method: str, path: str, body=None) -> dict[str, Any]:
        headers = {"Accept": "application/json"}
        payload = None
        if method == "POST":
            payload = json.dumps(body, allow_nan=False).encode()
            headers["Content-Type"] = "application/json"
            headers["X-Selfhost-Token"] = self._request("GET", "/api/session")["token"]
        connection = http.client.HTTPConnection("127.0.0.1", self.port, timeout=self.timeout)
        try:
            connection.request(method, path, payload, headers)
            response = connection.getresponse()
            raw = response.read()
            try:
                data = json.loads(raw)
            except (ValueError, UnicodeError):
                raise ApiError(response.status, "Daemon returned invalid JSON") from None
            if not 200 <= response.status < 300:
                message = data.get("error", response.reason) if isinstance(data, dict) else response.reason
                raise ApiError(response.status, str(message))
            if not isinstance(data, dict):
                raise ApiError(response.status, "Expected a JSON object")
            return data
        finally:
            connection.close()

    @staticmethod
    def _service_path(service_id: str) -> str:
        if not re.fullmatch(r"[a-z][a-z0-9-]{0,47}", service_id):
            raise ValueError("Invalid service ID")
        return f"/api/services/{service_id}"

    def health(self) -> dict[str, Any]:
        """Daemon identity, version, and workspace/configuration paths."""
        return self._request("GET", "/api/health")

    def state(self) -> dict[str, Any]:
        """Current services, telemetry, projects, budgets, and events."""
        return self._request("GET", "/api/state")

    def logs(self, service_id: str, tail: int = 200) -> str:
        """Return the last 1–2000 log lines as text (not a live stream)."""
        if type(tail) is not int or not 1 <= tail <= 2000:
            raise ValueError("tail must be an integer between 1 and 2000")
        return self._request("GET", f"{self._service_path(service_id)}/logs?tail={tail}")["logs"]

    def action(self, service_id: str, action: str, *, limits=None) -> dict[str, Any]:
        """Start, stop, restart, run a job, or apply optional service limits."""
        if action not in ("start", "stop", "restart", "run", "apply"):
            raise ValueError("Unknown service action")
        if limits is not None and action != "apply":
            raise ValueError("limits are only supported by apply")
        return self._request("POST", f"{self._service_path(service_id)}/{action}",
                             {} if limits is None else {"limits": limits})

    def deploy(self, service: dict, *, apply: bool = False, github=None) -> dict[str, Any]:
        """Preview a service declaration. Set apply=True to deploy it."""
        body = {"service": service, "apply": apply}
        if github is not None:
            body["github"] = github
        return self._request("POST", "/api/services", body)

    def create_project(self, project: dict) -> dict[str, Any]:
        """Create a named workspace group with id, name, and optional description."""
        return self._request("POST", "/api/projects", project)

    def set_budget(self, limits: dict) -> dict[str, Any]:
        """Apply the global CPU/memory budget."""
        return self._request("POST", "/api/budget", limits)

    def allocation_plan(self) -> dict[str, Any]:
        """Read the current weighted allocation plan."""
        return self._request("GET", "/api/allocation")

    def configure_allocation(self, *, enabled=None, budget=None, apply=False) -> dict[str, Any]:
        """Preview allocation changes; set apply=True to commit them."""
        body = {"apply": apply}
        if enabled is not None:
            body["enabled"] = enabled
        if budget is not None:
            body["budget"] = budget
        return self._request("POST", "/api/allocation", body)
