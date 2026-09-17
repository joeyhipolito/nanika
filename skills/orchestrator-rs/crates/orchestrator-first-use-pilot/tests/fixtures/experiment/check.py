#!/usr/bin/python3
"""Bounded operator acceptance: fake executables, zero real provider calls."""
import json
import os
import pathlib
import shutil
import subprocess
import sys

sources = pathlib.Path(__file__).resolve().parent
fixture = pathlib.Path(sys.argv[1]).resolve()
fixture.mkdir()
runner = pathlib.Path(sys.argv[2]).resolve()
results = []

def check(scenario):
    base = fixture / scenario
    base.mkdir()
    repo = base / "repo"
    repo.mkdir()
    (repo / "note.txt").write_text("before\n")
    for args in [["init", "-q"], ["add", "note.txt"], ["-c", "user.name=Fixture", "-c", "user.email=fixture@invalid", "commit", "-qm", "fixture"]]:
        subprocess.run(["/usr/bin/git", *args], cwd=repo, check=True, capture_output=True, env=dict(os.environ, GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL="/dev/null", GIT_TERMINAL_PROMPT="0"))
    for src, dest in [("fake-pilot.py", "pilot"), ("fake-provider.py", "provider"), ("verify.py", "verify"),
                      ("fake-provider.py", "orchestrator-output"), ("fake-provider.py", "orchestrator-process-broker")]:
        shutil.copyfile(sources / src, base / dest)
        (base / dest).chmod(0o700)
    (base / "scenario").write_text(scenario)
    (base / "prompt.md").write_text("Perform the fixture edit.")
    output = base / "experiment"
    args = [str(runner), "--allow-provider-experiments", "--pilot", str(base / "pilot"),
            "--provider", str(base / "provider"), "--repo", str(repo), "--prompt-file", str(base / "prompt.md"),
            "--output-dir", str(output), "--model", "fixture", "--persona", "general-purpose", "--pairs", "2",
            "--timeout-secs", "2", "--verification-timeout-secs", "2", "--", str(base / "verify"), "literal; $(not-a-command)"]
    if scenario == "no-opt-in":
        args.remove("--allow-provider-experiments")
    if scenario == "duplicate":
        args[1:1] = ["--pairs", "2"]
    result = subprocess.run(args, capture_output=True, timeout=90)
    (base / "stdout").write_bytes(result.stdout)
    (base / "stderr").write_bytes(result.stderr)
    if scenario in ["no-opt-in", "duplicate"]:
        assert result.returncode != 0, scenario
        assert not output.exists(), scenario
        assert not (base / "invocations.jsonl").exists(), scenario
    else:
        assert result.returncode == 0, (scenario, result.stderr)
        report = json.loads((output / "report.json").read_text())
        assert report["savings_claim"] is False
        assert report["cache_condition"] == "external/uncontrolled"
        samples = report["samples"]
        assert report["summary"]["off"]["planned_count"] == 2
        assert report["summary"]["on"]["planned_count"] == 2
        assert [s["arm"] for s in samples] == ["off", "on", "on", "off"]
        assert len(samples) == 4
        if scenario == "source-drift":
            assert samples[0]["quality_passed"] is False
            assert all(s["status"] == "not-run" for s in samples[1:])
        else:
            assert all(s["status"] == "sample_completed" for s in samples), (scenario, samples)
            if scenario in ["success", "missing-metrics"]:
                assert all(s["quality_passed"] for s in samples), (scenario, samples)
            if scenario in ["provider-failure", "verification-failure"]:
                assert not any(s["quality_passed"] for s in samples), scenario
            if scenario == "provider-failure":
                assert all(s["verification"]["success"] for s in samples), scenario
            if scenario == "no-application":
                assert [s["quality_passed"] for s in samples] == [True, False, False, True]
            if scenario == "missing-metrics":
                assert all(s["metrics"] is None for s in samples)
            assert all(s["full_command_log_bytes"] is None for s in samples if s["arm"] == "off")
            assert all(s["portal_failed_command_count"] == 1 for s in samples if s["arm"] == "on")
        assert (output / "report.json").stat().st_mode & 0o777 == 0o600
        assert output.stat().st_mode & 0o777 == 0o700
    results.append({"scenario": scenario, "passed": True, "returncode": result.returncode})
    fixture.with_suffix(".json").write_text(json.dumps(results, indent=2))

for scenario in ["no-opt-in", "duplicate", "success", "provider-failure", "verification-failure", "missing-metrics", "no-application", "source-drift"]:
    check(scenario)
print(json.dumps(results, indent=2))
