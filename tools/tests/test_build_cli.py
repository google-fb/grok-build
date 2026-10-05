"""Exercise the actual driver with a synthetic clone and Docker boundary."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


DRIVER = Path(__file__).resolve().parents[1] / "build-cli.sh"


class BuildCliTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="astra-build-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.src = self.root / "source with spaces"
        self.src.mkdir()
        self.git("init", "-q")
        self.git("config", "user.name", "Build Test")
        self.git("config", "user.email", "build@example.invalid")
        (self.src / "tracked").write_text("baseline\n")
        self.git("add", "tracked")
        self.git("commit", "-qm", "baseline")
        self.commit = self.git("rev-parse", "HEAD").stdout.strip()
        self.seed = self.root / "seed"
        (self.seed / "cargo").mkdir(parents=True)
        (self.seed / "cargo" / "sentinel").write_text("do not change")
        self.bin = self.root / "fake-bin"
        self.bin.mkdir()
        docker = self.bin / "docker"
        docker.write_text('''#!/usr/bin/env python3
import hashlib, json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ["DOCKER_CALLS"], "a") as f:
    f.write(json.dumps(args) + "\\n")
if args[0] == "image":
    print("sha256:mock-builder")
    sys.exit(0)
if os.environ.get("FAIL_BUILD"):
    sys.exit(17)
mounts = {}
for i, value in enumerate(args):
    if value == "--mount":
        fields = dict(x.split("=", 1) for x in args[i+1].split(",") if "=" in x)
        mounts[fields["target"]] = pathlib.Path(fields["source"])
out = mounts["/artifacts"]
commit = (out / "SOURCE-COMMIT.txt").read_text().strip()
payload = b"synthetic binary, never executed"
(out / "grok").write_bytes(payload)
(out / "VERSION.txt").write_text("grok 1.0.12 (" + commit[:12] + ")\\n")
(out / "SHA256SUMS").write_text(hashlib.sha256(payload).hexdigest() + "  grok\\n")
if os.environ.get("BAD_HASH"):
    (out / "grok").write_bytes(b"corrupted")
if os.environ.get("BAD_VERSION"):
    (out / "VERSION.txt").write_text("grok 1.0.12 (wrongcommit)\\n")
''')
        docker.chmod(0o755)
        self.calls = self.root / "docker-calls.jsonl"
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ["PATH"],
                        DOCKER_CALLS=str(self.calls))
        self.work = self.root / "work with spaces"
        self.out = self.root / "output with spaces"

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.src), *args], check=True,
                              text=True, capture_output=True)

    def run_driver(self, revision=None):
        return subprocess.run(["bash", str(DRIVER), str(self.src), str(self.seed),
                               str(self.work), str(self.out), revision or self.commit],
                              env=self.env, text=True, capture_output=True)

    def assert_rejected_before_docker(self, result):
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertFalse(self.calls.exists(), result.stderr)

    def test_explicit_revision_readonly_inputs_and_fresh_target(self):
        (self.src / "tracked").write_text("later\n")
        self.git("commit", "-qam", "later")
        before = self.git("rev-parse", "HEAD").stdout
        result = self.run_driver()
        self.assertEqual(result.returncode, 0, result.stderr)
        manifest = json.loads((self.out / "BUILD-MANIFEST.json").read_text())
        self.assertEqual(manifest["source_commit"], self.commit)
        self.assertFalse(manifest["matches_paper_binary"])
        self.assertFalse(manifest["compiled_cache_reused"])
        self.assertEqual(self.git("rev-parse", "HEAD").stdout, before)
        self.assertEqual((self.seed / "cargo" / "sentinel").read_text(), "do not change")
        self.assertEqual((self.work / "src" / "tracked").read_text(), "baseline\n")
        run = [json.loads(line) for line in self.calls.read_text().splitlines()][-1]
        mounts = [run[i+1] for i, val in enumerate(run) if val == "--mount"]
        self.assertTrue(any("target=/src,readonly" in m for m in mounts))
        self.assertTrue(any("target=/seed,readonly" in m for m in mounts))
        self.assertTrue(run[run.index("--name")+1].startswith("astra-"))
        self.assertIn("--pull=never", run)
        self.assertEqual(list((self.work / "cache").iterdir()), [])

    def test_existing_output_never_overwritten(self):
        self.out.mkdir()
        marker = self.out / "grok"
        marker.write_text("published")
        self.assert_rejected_before_docker(self.run_driver())
        self.assertEqual(marker.read_text(), "published")

    def test_existing_work_never_reused(self):
        self.work.mkdir()
        self.assert_rejected_before_docker(self.run_driver())

    def test_dirty_tracked_and_untracked_source(self):
        for name in ["tracked", "untracked"]:
            with self.subTest(name=name):
                (self.src / name).write_text("dirty")
                self.assert_rejected_before_docker(self.run_driver())
                if name == "tracked":
                    self.git("restore", "tracked")

    def test_invalid_revision(self):
        self.assert_rejected_before_docker(self.run_driver("missing-revision"))

    def test_missing_cache(self):
        self.seed = self.root / "missing-cache"
        self.assert_rejected_before_docker(self.run_driver())

    def test_output_inside_seed_and_source_rejected(self):
        for parent in [self.seed, self.src]:
            with self.subTest(parent=parent.name):
                self.out = parent / "new-output"
                self.assert_rejected_before_docker(self.run_driver())
                self.assertFalse(self.out.exists())

    def test_symlink_and_overlapping_destinations(self):
        self.out.symlink_to(self.root / "nonexistent")
        self.assert_rejected_before_docker(self.run_driver())
        self.out.unlink()
        self.out = self.work / "nested"
        self.assert_rejected_before_docker(self.run_driver())

    def test_docker_mount_delimiter_rejected(self):
        self.work = self.root / "work,readonly"
        self.assert_rejected_before_docker(self.run_driver())

    def test_failed_build_and_invalid_artifacts_not_published(self):
        for mode in ["FAIL_BUILD", "BAD_HASH", "BAD_VERSION"]:
            with self.subTest(mode=mode):
                self.work = self.root / mode
                self.env[mode] = "1"
                result = self.run_driver()
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse(self.out.exists())
                del self.env[mode]


if __name__ == "__main__":
    unittest.main()
