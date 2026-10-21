#!/usr/bin/env python3
"""Hermetic mechanism comparison; no model, credentials, token or quality claims.

Build: cargo build --locked -p xai-grok-tools --example code_mode_worker
Run: python3 scripts/bench-code-mode.py target/debug/examples/code_mode_worker
"""
import json
import statistics
import struct
import subprocess
import sys
import time


def data_bytes(value):
    return len(json.dumps(value, separators=(",", ":")).encode())


def invoke(name, args):
    if name == "lookup":
        assert args == {"query": "example"}
        return {"id": 7}
    if name == "read":
        assert args == {"id": 7}
        return [{"id": i, "score": i % 3, "detail": "x" * 300} for i in range(100)]
    if name == "details":
        assert args == {"ids": [0, 1, 3, 4, 6]}
        return {"selected": args["ids"]}
    raise AssertionError(name)


CODE = """
const entry = JSON.parse(await tools.lookup({query:'example'}));
const rows = JSON.parse(await tools.read({id:entry.id}));
const ids = rows.filter(row=>row.score<2).slice(0,5).map(row=>row.id);
return JSON.parse(await tools.details({ids}));
"""


def send(pipe, value):
    payload = json.dumps(value).encode()
    pipe.write(struct.pack("<I", len(payload)) + payload)
    pipe.flush()


def receive(pipe):
    header = pipe.read(4)
    if len(header) != 4:
        raise AssertionError("worker ended without result")
    size = struct.unpack("<I", header)[0]
    assert size <= 2 * 1024 * 1024
    return json.loads(pipe.read(size))


def main(executable):
    lookup = invoke("lookup", {"query": "example"})
    rows = invoke("read", {"id": lookup["id"]})
    ids = [row["id"] for row in rows if row["score"] < 2][:5]
    expected = invoke("details", {"ids": ids})
    times = []
    for _ in range(10):
        started = time.perf_counter()
        with subprocess.Popen([executable], stdin=subprocess.PIPE, stdout=subprocess.PIPE) as worker:
            send(worker.stdin, {"code": CODE, "catalog": [{"name": name} for name in ("lookup", "read", "details")]})
            calls = []
            while True:
                message = receive(worker.stdout)
                if message["type"] == "finished":
                    assert message["ok"] and message["value"] == expected, message
                    break
                calls.append(message["name"])
                value = invoke(message["name"], message["arguments"])
                # Production host exposes tool prompt text as a string.
                send(worker.stdin, {"id": message["id"], "ok": True, "payload": json.dumps(json.dumps(value))})
            worker.wait(timeout=5)
            assert worker.returncode == 0
            assert calls == ["lookup", "read", "details"]
        times.append((time.perf_counter() - started) * 1000)
    print(json.dumps({
        "kind": "scripted mechanism comparison; no live model",
        "samples": len(times), "correct": len(times),
        "inner_calls_each_side": 3,
        "dependent_tool_submission_steps": {"direct_scripted_baseline": 3, "code_mode": 1},
        "baseline_tool_result_payload_bytes": sum(map(data_bytes, [lookup, rows, expected])),
        "code_mode_return_value_bytes": data_bytes(expected),
        "worker_with_fake_host_median_ms": round(statistics.median(times), 3),
        "limits": "excludes code/catalog/trace envelope, actual model time, tokens, task success rate, and real tool latency",
    }, indent=2))


if __name__ == "__main__":
    main(sys.argv[1])
