#!/usr/bin/env python3
"""Reuse compiler objects from trusted branch jobs; never reuse release verdicts."""
import json
import os
import re
import subprocess
import sys
from urllib.parse import quote

PREFIX = "compiler-cache-v1-"
WORKFLOWS = {
    ".github/workflows/ci.yml", ".github/workflows/release-cache.yml",
    ".github/workflows/auto-release.yml", ".github/workflows/release.yml",
}


def trusted(artifact, run, repository, branches):
    source = artifact.get("workflow_run", {})
    return (type(artifact.get("id")) is int and artifact["id"] > 0
            and artifact.get("expired") is False
            and source.get("head_branch") in branches
            and type(source.get("repository_id")) is int
            and source.get("repository_id") == run.get("repository", {}).get("id")
            and re.fullmatch(r"[0-9a-f]{40}", source.get("head_sha", "")) is not None
            and source.get("repository_id") == source.get("head_repository_id")
            and run.get("repository", {}).get("full_name", "").lower() == repository.lower()
            and (run.get("head_repository") or {}).get("full_name", "").lower() == repository.lower()
            and run.get("head_branch") == source.get("head_branch")
            and run.get("head_sha") == source.get("head_sha")
            and run.get("id") == source.get("id")
            and run.get("event") in ("push", "workflow_dispatch")
            and run.get("path") in WORKFLOWS)


def candidates(artifacts, repository, branches, get_run, name=None):
    for artifact in sorted(artifacts, key=lambda a: (a.get("created_at", ""), a.get("id", 0)), reverse=True):
        if not artifact.get("name", "").startswith(PREFIX) or (name and artifact["name"] != name):
            continue
        source = artifact.get("workflow_run", {})
        if source.get("head_branch") not in branches or artifact.get("expired") is not False:
            continue
        run_id = source.get("id")
        if type(run_id) is not int or run_id <= 0:
            continue
        if trusted(artifact, get_run(run_id), repository, branches):
            yield artifact


def obsolete(artifacts, keep=2):
    groups = {}
    for artifact in artifacts:
        key = artifact["name"], artifact["workflow_run"]["head_branch"]
        groups.setdefault(key, []).append(artifact)
    return [artifact for group in groups.values()
            for artifact in sorted(group, key=lambda a: (a["created_at"], a["id"]), reverse=True)[keep:]]


def api(endpoint, paginate=False):
    args = ["gh", "api", endpoint]
    if paginate:
        args += ["--paginate", "--slurp"]
    return json.loads(subprocess.check_output(args, text=True))


def main():
    repository = os.environ["GH_REPO"]
    endpoint = "repos/" + repository + "/actions/artifacts"
    runs = {}

    def get_run(run_id):
        if run_id not in runs:
            runs[run_id] = api(f"repos/{repository}/actions/runs/{run_id}")
        return runs[run_id]

    if sys.argv[1:] == ["prune"]:
        artifacts = [a for page in api(endpoint + "?per_page=100", True) for a in page["artifacts"]]
        old = obsolete(candidates(artifacts, repository, {"main", "alpha"}, get_run))
        for artifact in old:
            result = subprocess.run(["gh", "api", "--method", "DELETE", endpoint + "/" + str(artifact["id"])],
                                    capture_output=True, text=True)
            if result.returncode and "HTTP 404" not in result.stderr:
                raise RuntimeError(result.stderr)
            print(f"Removed old compiler snapshot {artifact['id']}: {artifact['name']}")
        print(f"Kept the newest two compiler snapshots per workload and release branch; removed {len(old)}")
        return

    name = os.environ["CACHE_NAME"]
    if not name.startswith(PREFIX):
        raise ValueError("unexpected compiler snapshot name")
    branch = os.environ["CACHE_BRANCH"]
    selected = None
    try:
        artifacts = [a for page in api(endpoint + "?per_page=100&name=" + quote(name, safe=""), True)
                     for a in page["artifacts"]]
        selected = next(candidates(artifacts, repository, {"main", branch}, get_run, name), None)
    except (subprocess.CalledProcessError, OSError, KeyError, TypeError, ValueError):
        print("Could not verify a compiler snapshot; compiling with an empty local cache")
    with open(os.environ["GITHUB_OUTPUT"], "a") as output:
        if selected:
            output.write(f"artifact_id={selected['id']}\nrun_id={selected['workflow_run']['id']}\n")
    print(f"Restoring trusted compiler snapshot {selected['id']} from {selected['workflow_run']['head_branch']}"
          if selected else "No compatible trusted compiler snapshot found")


if __name__ == "__main__":
    main()
