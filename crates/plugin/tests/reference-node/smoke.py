# A second runtime joins the protocol with no host change.
#
# This is deliberately not a second reference plugin: that would be a second
# maintenance surface bought to re-prove a property the protocol already
# guarantees. What this proves is that nothing Rust leaks into the wire format —
# the whole state machine below is one dict and a loop.
#
# The rules a new language has to get right: stdout carries frames only (stderr is
# for humans); every reply echoes the request id exactly; `initialized` is a
# notification with no id and needs no answer; `shutdown` is a request, so answer
# it, flush, then exit 0.
import json
import sys

TABLE = {
    "initialize": {"protocolVersion": 1, "name": "smoke", "version": "1.0.0",
                   "capabilities": {"tools": True, "prompts": False, "hooks": []}},
    "tools/list": {"tools": [{"name": "hello", "description": "says hello",
                              "schema": {"type": "object"}}]},
    "tools/call": {"output": "hello from python", "isError": False},
    "shutdown": {},
}

for line in sys.stdin:
    request = json.loads(line)
    result = TABLE.get(request.get("method"))
    if result is None:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
    if request["method"] == "shutdown":
        sys.exit(0)
