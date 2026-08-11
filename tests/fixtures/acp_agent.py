import json
import sys
from pathlib import Path


def send(message):
    print(json.dumps(message, separators=(",", ":")), flush=True)


sessions = set()
pending = {}
cancelled = set()
next_session = 1
next_permission = 900

for line in sys.stdin:
    message = json.loads(line)
    method = message.get("method")
    request_id = message.get("id")

    if method == "initialize":
        agent_name = "claude-test" if any("--claude" in arg for arg in sys.argv) else "test-acp-agent"
        session_capabilities = {"resume": {}, "close": {}}
        if "--no-close" in sys.argv:
            session_capabilities.pop("close")
        send({
            "jsonrpc": "2.0",
            "id": request_id,
            "result": {
                "protocolVersion": 0 if "--protocol-zero" in sys.argv else 1,
                "agentCapabilities": {
                    "sessionCapabilities": session_capabilities
                },
                "agentInfo": {"name": agent_name, "version": "1.0.0"},
            },
        })
        if "--exit-after-init" in sys.argv:
            break
        if "--secret-error" in sys.argv:
            print("SECRET_PROVIDER_OUTPUT", file=sys.stderr, flush=True)
            sys.exit(7)
    elif method == "session/new":
        if "--hang-new" in sys.argv or (
            "--hang-new-after-first" in sys.argv and next_session > 1
        ):
            continue
        session_id = (
            "session-1"
            if "--duplicate-session-id" in sys.argv
            else f"session-{next_session}"
        )
        next_session += 1
        sessions.add(session_id)
        result = {"sessionId": session_id}
        if "--claude-mode" in sys.argv:
            result["modes"] = {
                "currentModeId": "auto",
                "availableModes": [
                    {"id": "auto", "name": "Auto"},
                    {"id": "default", "name": "Default"},
                ],
            }
        send({"jsonrpc": "2.0", "id": request_id, "result": result})
    elif method == "session/set_mode":
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "session/resume":
        session_id = message["params"]["sessionId"]
        if session_id not in sessions:
            send({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32002, "message": "Session not found"},
            })
        else:
            send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif method == "session/prompt":
        session_id = message["params"]["sessionId"]
        if "--auth-error" in sys.argv:
            send({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32000, "message": "Login required"},
            })
            continue
        permission_id = next_permission
        next_permission += 1
        pending[permission_id] = (request_id, session_id)
        if "--permission-after-cancel" in sys.argv:
            continue
        send({
            "jsonrpc": "2.0",
            "id": permission_id,
            "method": "session/request_permission",
            "params": {
                "sessionId": session_id,
                "toolCall": {"toolCallId": f"tool-{session_id}", "title": "Write file"},
                "options": [
                    {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
                    {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"},
                ],
            },
        })
    elif method == "session/cancel":
        session_id = message["params"]["sessionId"]
        cancelled.add(session_id)
        if "--permission-after-cancel" in sys.argv:
            permission_id = next(
                permission_id
                for permission_id, (_, pending_session) in pending.items()
                if pending_session == session_id
            )
            send({
                "jsonrpc": "2.0",
                "id": permission_id,
                "method": "session/request_permission",
                "params": {
                    "sessionId": session_id,
                    "toolCall": {"toolCallId": f"tool-{session_id}", "title": "Write file"},
                    "options": [
                        {"optionId": "allow-once", "name": "Allow once", "kind": "allow_once"},
                        {"optionId": "reject-once", "name": "Reject", "kind": "reject_once"},
                    ],
                },
            })
    elif method == "session/close":
        sessions.discard(message["params"]["sessionId"])
        send({"jsonrpc": "2.0", "id": request_id, "result": {}})
    elif request_id in pending and ("result" in message or "error" in message):
        for argument in sys.argv:
            if argument.startswith("--permission-result-file="):
                Path(argument.split("=", 1)[1]).write_text(
                    json.dumps(message.get("result")), encoding="utf-8"
                )
        if "--ignore-permission-response" in sys.argv:
            continue
        prompt_id, session_id = pending.pop(request_id)
        permission_cancelled = (
            "--cancelled-permission-stops-prompt" in sys.argv
            and "cancelled" in json.dumps(message.get("result"))
        )
        text = (
            "cancelled"
            if session_id in cancelled or permission_cancelled
            else "fixture reply"
        )
        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": text},
                },
            },
        })

        send({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": session_id,
                "update": {"sessionUpdate": "usage_update", "used": 12, "size": 4096},
            },
        })
        send({
            "jsonrpc": "2.0",
            "id": prompt_id,
            "result": {
                "stopReason": "cancelled" if session_id in cancelled else "end_turn"
            },
        })
