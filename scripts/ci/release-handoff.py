#!/usr/bin/env python3
"""Validate a coordinator's exact source and successful jobs before reusing it."""
import json
import os
import re
import subprocess
import time
from pathlib import Path


class HandoffPending(ValueError):
    """Trusted run whose completed jobs/artifacts are not yet visible."""


def validate(run, jobs, artifacts, repository, commit, targets):
    expected_branch = "alpha" if run.get("head_branch") == "alpha" else "main"
    if (run.get("repository", {}).get("full_name", "").lower() != repository.lower()
            or run.get("head_sha") != commit
            or run.get("head_branch") != expected_branch
            or run.get("event") != "workflow_dispatch"
            or run.get("path") != ".github/workflows/auto-release.yml"
            or run.get("status") not in ("in_progress", "completed")
            or run.get("conclusion") not in (None, "success")):
        raise ValueError("warm run is not the trusted coordinator for this exact release commit")
    required = {"Validate exact release source / CI required"}
    required.update("Warm exact release binaries / Warm " + target for target in targets)
    for name in required:
        matches = [job for job in jobs if job.get("name") == name]
        if not matches:
            raise HandoffPending("job not yet visible: " + name)
        if (len(matches) != 1 or matches[0].get("head_sha") != commit
                or matches[0].get("run_attempt") != run.get("run_attempt")):
            raise ValueError("invalid current-attempt job: " + name)
        if matches[0].get("status") != "completed" or matches[0].get("conclusion") is None:
            raise HandoffPending("job completion not yet visible: " + name)
        if matches[0].get("conclusion") != "success":
            raise ValueError("unsuccessful current-attempt job: " + name)
    for target in targets:
        matches = [item for item in artifacts if item.get("name") == "release-inputs-" + target]
        if not matches:
            raise HandoffPending("artifact not yet visible: " + target)
        if len(matches) != 1 or matches[0].get("expired") is not False:
            raise ValueError("missing unexpired release artifact: " + target)


def api(endpoint, paginate=False):
    args = ["gh", "api", endpoint]
    if paginate:
        args += ["--paginate", "--slurp"]
    return json.loads(subprocess.check_output(args, text=True))


def successful_ci(run, jobs, repository, commit):
    return (run.get("repository", {}).get("full_name", "").lower() == repository.lower()
            and run.get("head_sha") == commit
            and run.get("head_branch") in ("main", "alpha")
            and run.get("event") in ("push", "workflow_dispatch")
            and run.get("path") == ".github/workflows/ci.yml"
            and run.get("status") == "completed"
            and run.get("conclusion") == "success"
            and any(j.get("name") == "CI required" and j.get("status") == "completed"
                    and j.get("conclusion") == "success" and j.get("head_sha") == commit
                    and j.get("run_attempt") == run.get("run_attempt") for j in jobs))


def find_ci():
    repository = os.environ["GH_REPO"]
    commit = os.environ["RELEASE_COMMIT"]
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("CI reuse requires an exact commit SHA")
    endpoint = f"repos/{repository}/actions/workflows/ci.yml/runs?head_sha={commit}&status=success&per_page=100"
    found = ""
    try:
        for page in api(endpoint, True):
            for run in page["workflow_runs"]:
                jobs = [j for p in api(f"repos/{repository}/actions/runs/{run['id']}/jobs?filter=latest&per_page=100", True)
                        for j in p["jobs"]]
                if successful_ci(run, jobs, repository, commit):
                    found = str(run["id"])
                    break
            if found:
                break
    except (subprocess.CalledProcessError, KeyError, ValueError):
        print("Could not verify previous CI; running the full quality gates")
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write("reuse=" + ("true" if found else "false") + "\n")
    print("Verified successful exact-commit CI run " + found if found else "No reusable successful CI found")


def wait_for_handoff(endpoint, repository, commit, targets, attempts=31):
    # Actions can expose completed steps before updating the job conclusion.
    # Retry visibility lag, never a source mismatch or a failed gate.
    for attempt in range(attempts):
        run = api(endpoint)
        jobs = [j for page in api(endpoint + "/jobs?filter=latest&per_page=100", True) for j in page["jobs"]]
        artifacts = [a for page in api(endpoint + "/artifacts?per_page=100", True) for a in page["artifacts"]]
        try:
            validate(run, jobs, artifacts, repository, commit, targets)
            return
        except HandoffPending as error:
            if attempt == attempts - 1:
                raise
            print(f"Waiting for coordinator API visibility: {error}", flush=True)
            time.sleep(30)


def main():
    run_id = os.environ["BUILD_RUN_ID"]
    if not re.fullmatch(r"[1-9][0-9]*", run_id):
        raise ValueError("build_run_id must be a numeric Actions run ID")
    repository = os.environ["GH_REPO"]
    endpoint = f"repos/{repository}/actions/runs/{run_id}"
    targets = json.loads((Path(__file__).resolve().parents[1] / "dist/targets.json").read_text())
    wait_for_handoff(endpoint, repository, os.environ["RELEASE_COMMIT"], targets)
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        output.write(f"build_run_id={run_id}\n")
    print(f"Reusing all six target builds and quality gates from validated run {run_id}")


if __name__ == "__main__":
    import sys
    find_ci() if sys.argv[1:] == ["find-ci"] else main()
