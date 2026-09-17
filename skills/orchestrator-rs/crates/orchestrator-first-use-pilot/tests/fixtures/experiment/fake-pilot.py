#!/usr/bin/python3
"""Deterministic no-provider experiment fixture; scenario is a sibling file."""
import json
import pathlib
import sys

here = pathlib.Path(__file__).resolve().parent
scenario = (here / "scenario").read_text().strip()
args = sys.argv[1:]
with (here / "invocations.jsonl").open("a") as stream:
    stream.write(json.dumps(args) + "\n")
if args == ["features"]:
    print(json.dumps({"schema": "nanika.features.v2", "implementation_revision": "fixture/v1"}))
    sys.exit(0)
assert args[0] == "code"
def flag(name):
    return args[args.index(name) + 1]
out = pathlib.Path(flag("--output-dir"))
out.mkdir()
workspace = out / "workspace"
workspace.mkdir()
(workspace / "note.txt").write_text("before\n" if scenario == "verification-failure" else "after\n")
arm = flag("--feature").split("=")[1]
portal = None if arm == "off" else {
    "schema": "nanika.portal-application.v1", "status": "observed", "requested": "on",
    "applied": scenario != "no-application", "verified_wrapped_calls": 0 if scenario == "no-application" else 2,
    "full_log_bytes": 1000, "returned_bytes": 400,
    "commands": [{"command_exit_code": 2}, {"command_exit_code": 0}],
}
result = {"status": "failed" if scenario == "provider-failure" else "completed",
          "provider_completed": scenario != "provider-failure", "runtime": "codex",
          "route": {"model": flag("--model"), "persona": flag("--persona"), "effort": "medium"},
          "codex_protocol": {"tool_failed": arm == "on", "command_count": 2},
          "process": {"stdout_bytes": 1234}, "portal_application": portal}
features = {"schema": "nanika.run-features.v2", "runtime": "codex", "command": "code",
            "entries": [{"name": "portal-output-cap", "requested": arm, "effective": arm,
                         "source": "explicit-run-option", "supported": True, "applied": None}]}
usage = {"status": "available", "report": {"summary": {"input_tokens": 123,
         "output_tokens": 45, "cache_read_input_tokens": 0, "cache_creation_input_tokens": None,
         "reasoning_output_tokens": None, "cost_usd": None}}}
for name, value in [("pilot-result.json", result), ("run-features.json", features),
                    ("portal-application.json", portal), ("worker-usage.json", usage)]:
    if scenario == "missing-metrics" and name == "worker-usage.json":
        continue
    (out / name).write_text(json.dumps(value))
if scenario == "source-drift":
    (pathlib.Path(flag("--repo")) / "note.txt").write_text("drift\n")
sys.exit(1 if scenario == "provider-failure" else 0)
