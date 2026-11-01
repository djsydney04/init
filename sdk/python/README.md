# Self-host Init — Python SDK

Python 3.10+, standard library only. From the repository root:

```sh
python3 -m pip install ./sdk/python
selfhost serve
```

In another terminal:

```python
from selfhost import ApiError, Client

client = Client()  # port=8787, timeout=30 seconds per request
print(client.health())
print(client.state()["services"])

service = {
    "id": "worker",
    "image": "your-org/worker:1.0",  # replace with your image
    "limits": {"cpus": 1, "memory_mb": 256},
}
print(client.deploy(service))  # preview only
# After reviewing the plan:
# client.deploy(service, apply=True)
# print(client.logs("worker", tail=100))
# client.action("worker", "stop")

try:
    client.action("missing-service", "start")
except ApiError as error:
    print(error.status, error.message)
```

The client is synchronous. Responses are dictionaries; logs return text. Mutations retrieve a session token automatically and are never retried. After a timeout, inspect state before repeating an operation. Increase `timeout` for image pulls. No credentials or runtime dependencies are required beyond the local daemon.

[Complete operations, request fields, and connection behavior](https://github.com/djsydney04/self-host-init/blob/main/sdk/README.md).
