# Stand-in for `codex app-server` on stdio, used by the fake codex binaries of
# the tests: answers `hooks/list` with every managed hook trusted, from the
# config in CODEX_HOME; every other request gets an empty result.
import json
import os
import sys
from pathlib import Path

config = str((Path(os.environ["CODEX_HOME"]) / "config.toml").resolve())
events = [
    "SessionStart",
    "UserPromptSubmit",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "Interrupt",
    "SessionEnd",
]
for line in sys.stdin:
    try:
        request = json.loads(line)
    except ValueError:
        continue
    if "id" not in request:
        continue
    if request.get("method") == "hooks/list":
        hooks = [
            {"sourcePath": config, "key": e, "currentHash": "fixture", "trustStatus": "trusted"}
            for e in events
        ]
        result = {"data": [{"hooks": hooks}]}
    else:
        result = {}
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
