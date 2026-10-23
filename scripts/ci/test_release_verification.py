"""Exercise the actual parallel fan-in commands, especially failure propagation."""
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipIf(os.name == "nt", "The release fan-in runs on Linux with Bash/xargs")
class ReleaseVerificationTests(unittest.TestCase):
    def run_verification(self, failure=""):
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        start = workflow.index("          printf '%s\\0' \"${archives[@]}\"")
        end = workflow.index("          cp crates/codegen/xai-grok-pager/scripts/install.sh", start)
        commands = workflow[start:end]
        with tempfile.TemporaryDirectory(prefix="release-verify-test-") as temporary:
            folder = Path(temporary)
            (folder / "scripts").mkdir()
            archives = [f"archive {number}.tar.gz" for number in range(6)]
            for archive in archives:
                (folder / archive).touch()
                (folder / (archive + ".sigstore.json")).touch()
            if failure == "missing-bundle":
                (folder / (archives[2] + ".sigstore.json")).unlink()
            gh = folder / "gh"
            gh.write_text('''#!/usr/bin/env bash
set -euo pipefail
test "$1" = attestation
test "$2" = verify
printf 'signature:%s\\n' "$3" >> "$VERIFY_TEST_LOG"
if [[ "$VERIFY_TEST_FAILURE" == signature && "$3" == 'archive 2.tar.gz' ]]; then exit 11; fi
''')
            dist = folder / "scripts/dist.sh"
            dist.write_text('''#!/usr/bin/env bash
set -euo pipefail
test "$1" = verify
printf 'payload:%s\\n' "$2" >> "$VERIFY_TEST_LOG"
if [[ "$VERIFY_TEST_FAILURE" == payload && "$2" == 'archive 2.tar.gz' ]]; then exit 12; fi
''')
            gh.chmod(0o755)
            dist.chmod(0o755)
            log = folder / "checks.log"
            environment = dict(os.environ, PATH=str(folder) + os.pathsep + os.environ["PATH"],
                               VERIFY_TEST_LOG=str(log), VERIFY_TEST_FAILURE=failure,
                               GH_REPO="owner/repository", RELEASE_COMMIT="a" * 40)
            invocation = "set -euo pipefail\narchives=(" + " ".join(map(shlex.quote, archives)) + ")\n" + commands
            result = subprocess.run(["bash", "-c", invocation], cwd=folder, env=environment,
                                    capture_output=True, text=True, timeout=15)
            checks = log.read_text().splitlines() if log.exists() else []
            return result, checks, archives

    def test_all_six_signatures_and_payloads_are_checked(self):
        result, checks, archives = self.run_verification()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertCountEqual(checks, [kind + ":" + archive
                                     for kind in ("signature", "payload") for archive in archives])

    def test_any_missing_or_invalid_archive_blocks_publication(self):
        for failure in ("signature", "payload", "missing-bundle"):
            with self.subTest(failure=failure):
                result, checks, archives = self.run_verification(failure)
                self.assertNotEqual(result.returncode, 0)
                if failure != "payload":
                    self.assertNotIn("payload:" + archives[2], checks)


if __name__ == "__main__":
    unittest.main()
