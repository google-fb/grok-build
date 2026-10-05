"""Offline contract checks against a compiled production-backed Rust probe.

Set USAGE_SURFACES_BIN to the built usage_surfaces example executable.
All files below are newly generated synthetic fixtures in TemporaryDirectory.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class UsageSurfacesTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.binary = Path(os.environ["USAGE_SURFACES_BIN"]).resolve(strict=True)

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="astra-usage-test-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "fixture"
        self.env = {"PATH": os.environ["PATH"],
                    "GROK_HOME": str(Path(self.temp.name) / "empty-config")}

    def probe(self, mode):
        cmd = [str(self.binary), str(self.root), mode]
        timed_out = mode.endswith("timeout")
        if timed_out:
            cmd = ["timeout", "--signal=TERM", "2s", *cmd]
        result = subprocess.run(cmd, env=self.env, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 124 if timed_out else 0, result.stderr)
        ledgers = list(self.root.rglob("usage.json"))
        self.assertEqual(len(ledgers), 1)
        durable = json.loads(ledgers[0].read_text())
        updates = []
        for file in self.root.rglob("updates.jsonl"):
            updates.extend(json.loads(line) for line in file.read_text().splitlines() if line.strip())
        terminal = self.root / "terminal.json"
        return durable, updates, json.loads(terminal.read_text()) if terminal.exists() else None

    @staticmethod
    def completion(updates):
        # Adapter records use the timestamp/method/params disk envelope.
        return next(row["params"]["update"] for row in updates
                    if row.get("params", {}).get("update", {}).get("sessionUpdate") == "turn_completed")

    def test_normal_three_surfaces_nonzero_reasoning_and_disjoint_cache(self):
        durable, updates, terminal = self.probe("normal")
        wire = self.completion(updates)["usage"]
        totals = durable["totals"]
        self.assertEqual((totals["input_tokens"], totals["output_tokens"],
                          totals["cached_read_tokens"], totals["cache_creation_tokens"],
                          totals["reasoning_tokens"]), (160, 30, 50, 5, 10))
        self.assertEqual((wire["inputTokens"], wire["outputTokens"], wire["reasoningTokens"]),
                         (160, 30, 10))
        self.assertEqual(terminal["usage"], dict(input_tokens=105, output_tokens=30,
                         cache_read_input_tokens=50, cache_creation_input_tokens=5,
                         reasoning_tokens=10, total_tokens=190))
        self.assertEqual(terminal["total_cost_usd_ticks"], 3_000_000_000)
        self.assertEqual(terminal["total_cost_usd"], 0.3)
        self.assertEqual(terminal["modelUsage"]["model-a"]["inputTokens"], 65)
        self.assertEqual(wire["modelUsage"]["model-a"]["reasoningTokens"], 7)
        self.assertEqual(durable["by_model"]["model-b"]["reasoning_tokens"], 3)
        # A1 documents these baseline omissions; A2 must explicitly update this contract.
        self.assertNotIn("reasoningTokens", terminal["modelUsage"]["model-a"])
        self.assertNotIn("calls", durable)
        self.assertEqual(durable["main_loop_model_calls"], 2)

    def test_timeout_keeps_checkpoint_without_terminal_or_turn_completion(self):
        durable, updates, terminal = self.probe("timeout")
        self.assertEqual((self.root / "ready").read_text(), "second-checkpoint")
        self.assertIsNone(terminal)
        self.assertFalse(any(row.get("params", {}).get("update", {}).get("sessionUpdate") == "turn_completed" for row in updates))
        self.assertEqual(durable["totals"]["reasoning_tokens"], 10)
        self.assertEqual(durable["totals"]["cost_usd_ticks"], 3_000_000_000)
        # A checkpoint alone cannot certify that a killed phase completed.
        self.assertFalse(durable["incomplete"])

    def test_timeout_before_next_response_keeps_only_first_call(self):
        durable, _, terminal = self.probe("first-checkpoint-timeout")
        self.assertEqual((self.root / "ready").read_text(), "first-checkpoint")
        self.assertIsNone(terminal)
        self.assertEqual(durable["totals"]["model_calls"], 1)
        self.assertEqual(durable["totals"]["reasoning_tokens"], 7)
        self.assertEqual(durable["totals"]["cost_usd_ticks"], 1_000_000_000)
        self.assertEqual(set(durable["by_model"]), {"model-a"})

    def test_missing_cost_preserves_tokens_and_scrubs_wire_money(self):
        durable, updates, terminal = self.probe("missing-cost")
        self.assertEqual(durable["totals"]["cost_missing_calls"], 1)
        self.assertEqual(durable["totals"]["cost_usd_ticks"], 1_000_000_000)
        self.assertEqual(terminal["usage"]["reasoning_tokens"], 10)
        self.assertTrue(terminal["cost_is_partial"])
        self.assertNotIn("total_cost_usd", terminal)
        self.assertNotIn("costUsdTicks", self.completion(updates)["usage"])
        self.assertNotIn("costUSD", terminal["modelUsage"]["model-a"])

    def test_incomplete_ledger_scrubs_wire_money(self):
        durable, updates, terminal = self.probe("incomplete")
        self.assertTrue(durable["incomplete"])
        self.assertEqual(durable["totals"]["cost_usd_ticks"], 3_000_000_000)
        self.assertTrue(terminal["usage_is_incomplete"])
        self.assertNotIn("total_cost_usd_ticks", terminal)
        self.assertNotIn("costUsdTicks", self.completion(updates)["usage"])

    def test_refuses_existing_directory_and_unknown_mode(self):
        self.root.mkdir()
        marker = self.root / "untouched"
        marker.write_text("existing")
        result = subprocess.run([str(self.binary), str(self.root), "normal"], env=self.env, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(marker.read_text(), "existing")
        fresh = self.root.parent / "unknown-mode"
        result = subprocess.run([str(self.binary), str(fresh), "unknown"], env=self.env, capture_output=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(fresh.exists())


if __name__ == "__main__":
    unittest.main()
