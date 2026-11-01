import os
import socket
from selfhost import ApiError, Client

client = Client(port=int(os.environ["SELFHOST_TEST_PORT"]))
assert client.health()["app"] == "self-host-init"
assert client.state()["services"][0]["id"] == "worker"
assert client.logs("worker", 12) == "hello\n"
assert client.allocation_plan() == {"services": []}
for action in ("start", "stop", "restart", "run"):
    assert client.action("worker", action)["ok"]
limits = {"cpus": 1, "memory_mb": 256}
assert client.action("worker", "apply", limits=limits)["ok"]
service = {"id": "worker", "image": "example/worker:1", "limits": limits}
assert client.deploy(service)["applied"] is False
assert client.deploy(service, apply=True)["applied"] is True
assert client.create_project({"id": "dev", "name": "Development"})["ok"]
assert client.set_budget(limits)["ok"]
assert client.configure_allocation(enabled=True)["applied"] is False
assert client.configure_allocation(enabled=True, apply=True)["applied"] is True
for service_id, status in (("blocked", 409), ("expired", 403)):
    try:
        client.action(service_id, "start")
        raise AssertionError("Expected ApiError")
    except ApiError as error:
        assert error.status == status and error.message
for call in (lambda: Client(port=0), lambda: Client(timeout=0),
             lambda: client.logs("worker", 0), lambda: client.logs("../worker"),
             lambda: client.action("worker", "destroy"),
             lambda: client.action("worker", "start", limits=limits)):
    try:
        call()
        raise AssertionError("Expected ValueError")
    except ValueError:
        pass

try:
    Client(port=client.port, timeout=0.05).action("slow", "start")
    raise AssertionError("Expected timeout")
except (TimeoutError, socket.timeout):
    pass
