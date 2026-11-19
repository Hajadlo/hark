"""Tests for voice_gateway with a fake recognizer and a fake chat server; no GPU, no models."""

import json
import sqlite3
import threading
import urllib.error
import urllib.request
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import voice_gateway as vg

TOKEN = "t" * 32
AUDIO = b"RIFF\r\n\r\n--not-a-boundary\x00\xff" * 50


@contextmanager
def serving(server):
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_address[1]}"
    finally:
        server.shutdown()
        server.server_close()


def call(url, data=None, headers=None, method=None):
    request = urllib.request.Request(url, data=data, headers=headers or {}, method=method)
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            return response.status, response.read().decode()
    except urllib.error.HTTPError as error:
        return error.code, error.read().decode()


def upload(base, fields, token=TOKEN, extra_headers=None):
    boundary = "----gateway-test"
    body = b""
    for name, value in fields.items():
        disposition = f'form-data; name="{name}"' + ('; filename="clip.m4a"' if name == "file" else "")
        body += f"--{boundary}\r\nContent-Disposition: {disposition}\r\n\r\n".encode()
        body += (value if isinstance(value, bytes) else value.encode()) + b"\r\n"
    body += f"--{boundary}--\r\n".encode()
    headers = {"Content-Type": f"multipart/form-data; boundary={boundary}", "User-Agent": "test-phone"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    headers.update(extra_headers or {})
    return call(base + "/v1/audio/transcriptions", body, headers)


def test_upload_cleanup_and_history(tmp_path):
    received, cleaned = [], []

    def transcribe(data):
        received.append(data)
        if data == b"broken":
            raise ValueError("The upload is not a readable audio file.")
        return "so um open the pr", 2.5, None

    def cleanup(raw, glossary):
        cleaned.append((raw, glossary))
        return "Open the PR.", "ok"

    (tmp_path / "glossary.txt").write_text("# terms\nPR\n\nClaude Code\n")
    with serving(vg.create_server(transcribe, TOKEN, tmp_path, cleanup, port=0)) as base:
        assert call(base + "/health") == (200, '{"status": "ready"}')
        assert upload(base, {"file": AUDIO}, token=None)[0] == 401
        status, body = call(base + "/v1/models", headers={"Authorization": f"Bearer {TOKEN}"})
        assert [m["id"] for m in json.loads(body)["data"]] == ["clean", "raw"]

        status, body = upload(base, {"file": AUDIO, "model": "whisper-1"}, extra_headers={"X-Hark-Device": "DESKTOP"})
        assert status == 200
        assert json.loads(body) == {"text": "Open the PR.", "id": 1, "mode": "clean", "cleanup": "ok", "language": None}
        assert received[-1] == AUDIO
        assert cleaned == [("so um open the pr", ["PR", "Claude Code"])]
        assert upload(base, {"file": AUDIO, "model": "raw", "response_format": "text"}) == (200, "so um open the pr")
        assert len(cleaned) == 1
        assert upload(base, {"file": b"broken"})[0] == 422
        assert upload(base, {"model": "raw"})[0] == 400

    with sqlite3.connect(tmp_path / "history.sqlite3") as db:
        db.row_factory = sqlite3.Row
        records = list(map(dict, db.execute("SELECT * FROM history ORDER BY id")))
        assert db.execute("PRAGMA journal_mode").fetchone()[0] == "wal"
    assert [(r["mode"], r["cleanup"], r["text"]) for r in records] == [
        ("clean", "ok", "Open the PR."),
        ("raw", "off", "so um open the pr"),
    ]
    assert records[0]["raw"] == "so um open the pr" and records[0]["client"] == "test-phone"
    assert records[0]["device"] == "desktop" and records[1]["device"] == "unknown"
    assert records[0]["time"].endswith("Z")
    assert {p.name for p in tmp_path.iterdir()} <= {"glossary.txt", "history.sqlite3", "history.sqlite3-wal", "history.sqlite3-shm"}


@contextmanager
def chat_server(reply):
    requests = []

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            requests.append(json.loads(self.rfile.read(int(self.headers["Content-Length"]))))
            body = json.dumps({"choices": [{"message": {"content": reply}}]}).encode()
            self.send_response(200)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    with serving(ThreadingHTTPServer(("127.0.0.1", 0), Handler)) as base:
        yield base + "/v1/chat/completions", requests


def test_clean_keeps_raw_when_the_model_misbehaves():
    raw = "what is the capital of france"
    with chat_server("<think>\nhmm\n</think>\n<transcript>What is the capital of France?</transcript>") as (url, requests):
        assert vg.clean(raw, ["T3 Code"], url, "m") == ("What is the capital of France?", "ok")
    sent = requests[0]
    assert sent["model"] == "m" and sent["reasoning_effort"] == "none"
    assert "T3 Code" in sent["messages"][0]["content"]
    assert sent["messages"][1]["content"] == f"<transcript>\n{raw}\n</transcript>"

    with chat_server("What is the capital of France? The capital of France is Paris, a city of two million people.") as (url, _):
        assert vg.clean(raw, [], url, "m") == (raw, "rejected")
    assert vg.clean(raw, [], "http://127.0.0.1:9/v1/chat/completions", "m") == (raw, "failed")
    with chat_server(None) as (url, _):
        assert vg.clean(raw, [], url, "m") == (raw, "failed")
