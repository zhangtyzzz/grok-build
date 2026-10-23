#!/usr/bin/env python3
"""Dispatch warm-only work after exact, trusted alpha CI; no publication API."""
import json
import os
from pathlib import Path
import re
import subprocess
import time

CI = ".github/workflows/ci.yml"
WARM = ".github/workflows/release-cache.yml"
COORDINATOR = ".github/workflows/auto-release.yml"
SHA = re.compile(r"[0-9a-f]{40}")
ACTIVE = {"requested", "waiting", "pending", "queued", "in_progress"}


def same_repository(run, repository, repository_id):
    return all(type((run.get(field) or {}).get("id")) is int
               and (run.get(field) or {}).get("id") == repository_id
               and (run.get(field) or {}).get("full_name", "").lower() == repository.lower()
               for field in ("repository", "head_repository"))


def validate_ci(trigger, run, jobs, repository, repository_id):
    commit = trigger.get("head_sha", "")
    attempt = trigger.get("run_attempt")
    if (not isinstance(commit, str) or not SHA.fullmatch(commit)
            or type(trigger.get("id")) is not int
            or trigger["id"] <= 0 or type(attempt) is not int or attempt <= 0
            or trigger.get("conclusion") != "success" or trigger.get("status") != "completed"
            or trigger.get("event") not in ("push", "workflow_dispatch")
            or trigger.get("head_branch") != "alpha"
            or not same_repository(run, repository, repository_id)
            or run.get("id") != trigger["id"] or run.get("run_attempt") != attempt
            or run.get("head_sha") != commit or run.get("head_branch") != "alpha"
            or run.get("event") != trigger["event"] or run.get("path") != CI
            or run.get("status") != "completed" or run.get("conclusion") != "success"):
        raise ValueError("event is not the latest successful trusted alpha CI attempt")
    required = [j for j in jobs if j.get("name") == "CI required"]
    if (len(required) != 1 or required[0].get("head_sha") != commit
            or required[0].get("run_attempt") != attempt
            or required[0].get("status") != "completed"
            or required[0].get("conclusion") != "success"):
        raise ValueError("exact CI required gate did not succeed")
    return commit


def coordinator_warmed(api, prefix, run, commit):
    # An existing release tag makes the coordinator succeed with all warm jobs
    # skipped. Require actual successful builds, just as the handoff validator
    # does, without treating this cache hint as publication/handoff evidence.
    run_id, attempt = run.get("id"), run.get("run_attempt")
    if type(run_id) is not int or run_id <= 0 or type(attempt) is not int or attempt <= 0:
        return False
    jobs = [j for page in api(f"{prefix}/actions/runs/{run_id}/jobs?filter=latest&per_page=100",
                              paginate=True) for j in page["jobs"]]
    targets = json.loads((Path(__file__).resolve().parents[1] / "dist/targets.json").read_text())
    if not targets:
        return False
    for target in targets:
        matches = [j for j in jobs if j.get("name") == "Warm exact release binaries / Warm " + target]
        if (len(matches) != 1 or matches[0].get("head_sha") != commit
                or matches[0].get("run_attempt") != attempt
                or matches[0].get("status") != "completed"
                or matches[0].get("conclusion") != "success"):
            return False
    return True


def existing_warm(runs, repository, repository_id, commit, api, prefix):
    for run in runs:
        if (not same_repository(run, repository, repository_id)
                or run.get("head_branch") != "alpha" or run.get("head_sha") != commit
                or run.get("event") != "workflow_dispatch" or run.get("path") not in (WARM, COORDINATOR)):
            continue
        if run.get("status") in ACTIVE:
            return True
        if run.get("status") == "completed" and run.get("conclusion") == "success":
            if run.get("path") == WARM or coordinator_warmed(api, prefix, run, commit):
                return True
    return False


def warm_runs(api, prefix, commit):
    pages = api(f"{prefix}/actions/workflows/release-cache.yml/runs?branch=alpha"
                f"&event=workflow_dispatch&head_sha={commit}&per_page=100", paginate=True)
    return [r for p in pages for r in p["workflow_runs"]]


def confirm_dispatch(api, prefix, repository, repository_id, commit, known_ids,
                     dispatched_id, attempts=6, sleep=time.sleep):
    # Keep the dispatcher serialized until its new run is visible to the next
    # deduplication check. Never retry a POST whose outcome may be ambiguous.
    for attempt in range(attempts):
        runs = warm_runs(api, prefix, commit)
        for run in runs:
            run_id = run.get("id")
            if (type(run_id) is int and run_id > 0 and run_id not in known_ids
                    and (dispatched_id is None or run_id == dispatched_id)
                    and same_repository(run, repository, repository_id)
                    and run.get("head_branch") == "alpha" and run.get("head_sha") == commit
                    and run.get("event") == "workflow_dispatch" and run.get("path") == WARM):
                return run_id
        if attempt + 1 < attempts:
            sleep(5)
    raise ValueError("Warm dispatch accepted but not visible; inspect Actions before retrying")


def dispatch(api, event, repository, repository_id, sleep=time.sleep):
    if event.get("action") != "completed":
        raise ValueError("expected completed workflow_run event")
    trigger = event["workflow_run"]
    run_id = trigger.get("id")
    if type(run_id) is not int or run_id <= 0:
        raise ValueError("invalid CI run id")
    prefix = f"repos/{repository}"
    endpoint = f"{prefix}/actions/runs/{run_id}"
    run = api(endpoint)
    jobs = [j for page in api(endpoint + "/jobs?filter=latest&per_page=100", paginate=True)
            for j in page["jobs"]]
    commit = validate_ci(trigger, run, jobs, repository, repository_id)
    if api(prefix + "/commits/alpha").get("sha") != commit:
        return "Alpha advanced; skipping obsolete CI commit"
    known_ids = set()
    for workflow in ("release-cache.yml", "auto-release.yml"):
        pages = api(f"{prefix}/actions/workflows/{workflow}/runs?branch=alpha"
                    f"&event=workflow_dispatch&head_sha={commit}&per_page=100", paginate=True)
        runs = [r for p in pages for r in p["workflow_runs"]]
        known_ids.update(r["id"] for r in runs if type(r.get("id")) is int)
        if existing_warm(runs, repository, repository_id, commit, api, prefix):
            return "Exact alpha commit already warming or warmed"
    # Close ordinary branch/rerun races; downstream must also compare its own
    # GITHUB_SHA against the requested commit before any cache writes/builds.
    validate_ci(trigger, api(endpoint), jobs, repository, repository_id)
    if api(prefix + "/commits/alpha").get("sha") != commit:
        return "Alpha advanced before dispatch; skipping"
    result = api(prefix + "/actions/workflows/release-cache.yml/dispatches", method="POST",
                 payload={"ref": "alpha", "inputs": {"commit": commit}})
    run_id = result.get("workflow_run_id") if isinstance(result, dict) else None
    if run_id is not None and (type(run_id) is not int or run_id <= 0):
        raise ValueError("invalid warm dispatch response; inspect Actions before retrying")
    visible_id = confirm_dispatch(api, prefix, repository, repository_id, commit,
                                  known_ids, run_id, sleep=sleep)
    return f"Dispatched warm-only alpha {commit} as run {visible_id}"


def api(endpoint, method="GET", payload=None, paginate=False):
    command = ["gh", "api", "--method", method, endpoint]
    if paginate:
        command += ["--paginate", "--slurp"]
    if payload is not None:
        command += ["--input", "-"]
    result = subprocess.run(command, input=json.dumps(payload) if payload is not None else None,
                            text=True, stdout=subprocess.PIPE, check=True)
    return json.loads(result.stdout) if result.stdout.strip() else None


def main():
    if os.environ.get("GITHUB_EVENT_NAME") != "workflow_run":
        raise ValueError("only workflow_run may dispatch automatic warmup")
    repository = os.environ["GH_REPO"]
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", repository):
        raise ValueError("invalid repository")
    repository_id = int(os.environ["GITHUB_REPOSITORY_ID"])
    if repository_id <= 0:
        raise ValueError("invalid repository id")
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    print(dispatch(api, event, repository, repository_id))


if __name__ == "__main__":
    main()
