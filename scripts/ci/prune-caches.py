#!/usr/bin/env python3
"""Keep one immutable snapshot per cache workload and release branch."""
import json
import os
import re
import subprocess
import sys


def obsolete(caches):
    groups = {}
    retired = []
    for cache in caches:
        if cache.get("ref") not in ("refs/heads/main", "refs/heads/alpha"):
            continue
        key = cache.get("key", "")
        match = re.fullmatch(r"(sccache-disk-v1-.+)-[0-9a-f]{64}-[0-9a-f]{40}", key)
        if match:
            retired.append(cache)
            continue
        match = re.fullmatch(r"(cargo-v1-.+)-[0-9a-f]{64}", key)
        if match is None:
            match = re.fullmatch(r"(release-build-v3-.+)-[0-9a-f]{64}-[0-9a-f]{40}", key)
        if match:
            groups.setdefault((cache["ref"], match[1]), []).append(cache)
    return retired + [cache for group in groups.values()
            for cache in sorted(group, key=lambda c: (c["created_at"], c["id"]), reverse=True)[1:]]


def main():
    endpoint = "repos/" + os.environ["GH_REPO"] + "/actions/caches"
    caches = []
    # Do not enumerate thousands of content-addressed sccache/ objects.
    for prefix in ("sccache-disk-v1-", "cargo-v1-", "release-build-v3-"):
        pages = json.loads(subprocess.check_output(
            ["gh", "api", endpoint + "?per_page=100&key=" + prefix,
             "--paginate", "--slurp"], text=True))
        caches.extend(c for page in pages for c in page["actions_caches"])
    candidates = obsolete(caches)
    for cache in candidates:
        print(f"Obsolete snapshot {cache['id']}: {cache['ref']} / {cache['key']}", flush=True)
        if sys.argv[1:] == ["--delete"]:
            result = subprocess.run(
                ["gh", "api", "--method", "DELETE", endpoint + "/" + str(cache["id"])],
                capture_output=True, text=True)
            # GitHub may evict a cache after we list it. Other failures matter.
            if result.returncode and "HTTP 404" not in result.stderr:
                raise RuntimeError(result.stderr)
    print(f"{len(candidates)} obsolete snapshots; {sum(c['size_in_bytes'] for c in candidates)} bytes")


if __name__ == "__main__":
    main()
