import json
import os
import sys
import time
import subprocess
from pathlib import Path


def send(message):
    print(json.dumps(message, separators=(",", ":")), flush=True)


def complete_prompt(prompt_id, session_id, text="fixture reply", stop_reason="end_turn"):
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
        "result": {"stopReason": stop_reason},
    })


def call_room_mcp(config, arguments):
    env = dict(os.environ)
    env.update({item["name"]: item["value"] for item in config["env"]})
    requests = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26", "capabilities": {}, "clientInfo": {"name": "fixture", "version": "1"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
    ]
    for index, args in enumerate(arguments, start=3):
        requests.append({"jsonrpc": "2.0", "id": index, "method": "tools/call", "params": {"name": "send_room_message", "arguments": args}})
    result = subprocess.run([config["command"], *config["args"]], env=env,
        input="".join(json.dumps(request) + "\n" for request in requests), text=True, capture_output=True, timeout=15)
    assert result.returncode == 0, result.stderr
    return [json.loads(line) for line in result.stdout.splitlines()]


room_configs = {}
old_room_configs = {}
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
        room_configs[session_id] = message["params"].get("mcpServers", [])
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
        old_room_configs[session_id] = room_configs.get(session_id, [])
        room_configs[session_id] = message["params"].get("mcpServers", [])
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
        if prompt_log := os.environ.get("ACP_PROMPT_LOG"):
            content = message["params"]["prompt"][0]["text"]
            with Path(prompt_log).open("a") as log:
                log.write(json.dumps(content) + "\n")
        if "--room-mcp" in sys.argv:
            config = room_configs[session_id][0]
            args = {"targets": ["pay", "ops", "pay"], "body": "shared agent message", "request_id": "stable-key"}
            requests = [args, args, dict(args, body="conflicting"), dict(args, sender_id="forged"), dict(args, room_id="forged"), dict(args, targets=["outside"]), dict(args, targets=["missing"])]
            results = call_room_mcp(config, requests)
            forged = dict(config, env=[dict(item, value="wrong") if item["name"] == "JULY_ROOM_TOKEN" else item for item in config["env"]])
            results += call_room_mcp(forged, [args])[-1:]
            if old_room_configs.get(session_id):
                results += call_room_mcp(old_room_configs[session_id][0], [args])[-1:]
            log_path = next(arg.split("=", 1)[1] for arg in sys.argv if arg.startswith("--room-mcp-log="))
            with Path(log_path).open("a") as log:
                log.write(json.dumps({"config": config, "results": results}) + "\n")
        if "--slow-prompt" in sys.argv:
            time.sleep(0.05)
        if "--protocol-error" in sys.argv:
            send({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32603, "message": "Prompt failed"},
            })
            continue
        if "--auth-error" in sys.argv:
            send({
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32000, "message": "Login required"},
            })
            continue
        if "--no-permission" in sys.argv:
            complete_prompt(request_id, session_id)
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
        complete_prompt(
            prompt_id,
            session_id,
            text,
            "cancelled" if session_id in cancelled else "end_turn",
        )
