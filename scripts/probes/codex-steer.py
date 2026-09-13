#!/usr/bin/env python3
"""Codex app-server の実配送を隔離 thread で検証する（モデル利用あり）。

実行: python3 scripts/probes/codex-steer.py
結果と送受信 JSONL は表示される一時ディレクトリに保存する。
既存会話・VP daemon・ユーザー設定は変更しない。
"""

import json
from pathlib import Path
import queue
import subprocess
import tempfile
import threading
import time
import uuid


def main():
    root = Path(tempfile.mkdtemp(prefix="vp-steer-probe-"))
    print(f"Artifacts: {root}", flush=True)
    events = []
    incoming = queue.Queue()
    report = {"version": subprocess.check_output(["codex", "--version"], text=True).strip()}
    with (root / "wire.jsonl").open("w") as wire, (root / "stderr.log").open("w") as stderr:
        proc = subprocess.Popen(
            ["codex", "app-server"], stdin=subprocess.PIPE,
            stdout=subprocess.PIPE, stderr=stderr, text=True, bufsize=1,
        )

        def read():
            for line in proc.stdout:
                incoming.put(json.loads(line))
            incoming.put(None)

        threading.Thread(target=read, daemon=True).start()
        serial = 0

        def send(method, params, notification=False):
            nonlocal serial
            serial += 1
            message = {"method": method, "params": params}
            if not notification:
                message["id"] = serial
            wire.write(json.dumps({"send": message}) + "\n")
            wire.flush()
            proc.stdin.write(json.dumps(message) + "\n")
            proc.stdin.flush()
            return serial

        def wait_for(predicate, timeout=120):
            deadline = time.monotonic() + timeout
            while True:
                for index, event in enumerate(events):
                    if predicate(event):
                        return events.pop(index)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("期待した RPC 応答・通知が届きません")
                event = incoming.get(timeout=remaining)
                if event is None:
                    raise RuntimeError("app-server が終了しました")
                wire.write(json.dumps({"recv": event}) + "\n")
                wire.flush()
                events.append(event)

        def rpc(method, params):
            request_id = send(method, params)
            return wait_for(lambda event: event.get("id") == request_id)

        def text_input(text):
            return [{"type": "text", "text": text, "text_elements": []}]

        def completed(turn_id):
            event = wait_for(lambda e: e.get("method") == "turn/completed"
                             and e["params"]["turn"]["id"] == turn_id)
            assert event["params"]["turn"]["status"] == "completed", event
            return event

        try:
            init = rpc("initialize", {"clientInfo": {"name": "vp_steer_probe", "version": "1"},
                                      "capabilities": {"experimentalApi": True}})
            assert "result" in init, init
            send("initialized", {}, notification=True)
            started = rpc("thread/start", {
                "cwd": str(root), "ephemeral": True, "sandbox": "read-only",
                "developerInstructions": "This is a transport probe. Do not use tools, files, network, skills, or subagents. Only reply with the token requested by the latest user message.",
            })
            assert "result" in started, started
            thread_id = started["result"]["thread"]["id"]
            report["thread_id"] = thread_id
            report["model"] = started["result"]["model"]
            first = rpc("turn/start", {"threadId": thread_id, "input": text_input("Reply INITIAL.")})
            assert "result" in first, first
            turn_id = first["result"]["turn"]["id"]
            invalid = rpc("turn/steer", {"threadId": thread_id, "expectedTurnId": "wrong-turn",
                                         "input": text_input("Reply INVALID.")})
            assert "error" in invalid, invalid
            report["wrong_turn"] = invalid["error"]
            token = "STEER_" + uuid.uuid4().hex[:12]
            client_id = str(uuid.uuid4())
            steered = rpc("turn/steer", {"threadId": thread_id, "expectedTurnId": turn_id,
                                         "clientUserMessageId": client_id,
                                         "input": text_input(f"Reply exactly {token} instead.")})
            assert steered.get("result", {}).get("turnId") == turn_id, steered
            report["active_steer"] = steered["result"]
            print("Active steer accepted; waiting for committed output.", flush=True)
            completed(turn_id)
            messages = [e["params"]["item"] for e in events
                        if e.get("method") == "item/completed"
                        and e["params"].get("turnId") == turn_id]
            assert any(token in m.get("text", "") for m in messages
                       if m.get("type") == "agentMessage"), messages
            report["steer_token_observed"] = True
            report["client_id_observed"] = any(m.get("clientId") == client_id
                                                for m in messages if m.get("type") == "userMessage")
            late = rpc("turn/steer", {"threadId": thread_id, "expectedTurnId": turn_id,
                                      "input": text_input("Reply LATE.")})
            assert "error" in late, late
            report["completed_turn_steer"] = late["error"]
            next_token = "NEXT_" + uuid.uuid4().hex[:12]
            second = rpc("turn/start", {"threadId": thread_id,
                                        "input": text_input(f"Reply exactly {next_token}.")})
            assert "result" in second, second
            next_id = second["result"]["turn"]["id"]
            assert next_id != turn_id
            completed(next_id)
            assert any(e.get("method") == "item/completed"
                       and e["params"].get("turnId") == next_id
                       and next_token in e["params"]["item"].get("text", "") for e in events)
            report["idle_start_token_observed"] = True
            report["status"] = "passed"
        except BaseException as error:
            report["status"] = "failed"
            report["error"] = repr(error)
            raise
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()
            (root / "report.json").write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
            print(json.dumps(report, ensure_ascii=False, indent=2), flush=True)


if __name__ == "__main__":
    main()
