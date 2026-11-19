#!/usr/bin/env python3
"""Serve web/index.html locally with synthetic history, for working on the page.

Starts create_server() with a fake transcriber and fake cleanup on a temp state
directory seeded with synthetic dictations and the starter glossary, plus a
loopback proxy that adds the owner's Tailscale-User-Login header the way
Tailscale Serve would, so a normal browser can open the page without a token.
Nothing here touches the live gateway or the real history.

    python3 scripts/dev_page.py [--port 8780] [--owner dev@example.com]

Stop with Ctrl-C; the temp state directory is removed.
"""

import argparse
import http.client
import logging
import random
import shutil
import signal
import sys
import tempfile
import threading
from datetime import datetime, timedelta, timezone
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import voice_gateway as vg  # noqa: E402
from gateway_state import History, Modes  # noqa: E402

HERE = Path(__file__).resolve().parents[1]
TOKEN = "dev-page-" + "x" * 32

# (raw, final, mode, cleanup). Repeated mishearings give Corrections its rows.
SAMPLES = [
    ("so um open clod code and check the pr", "Open Claude Code and check the PR.", "clean", "ok"),
    ("ask clod to review the work tree please", "Ask Claude to review the worktree, please.", "clean", "ok"),
    ("run the tests in code x and then merge", "Run the tests in Codex and then merge.", "clean", "ok"),
    ("the tail scale serve port is eighty seven sixty six", "The Tailscale Serve port is 8766.", "clean", "ok"),
    ("uh restart olama on the desktop and pull the model again", "Restart Ollama on the desktop and pull the model again.", "clean", "ok"),
    ("clod said the history page is coming", "Claude said the history page is coming.", "clean", "ok"),
    ("check the graph ana dashboard before standup", "Check the Grafana dashboard before standup.", "clean", "ok"),
    ("the clod code worktree has no dot env yet", "The Claude Code worktree has no .env yet.", "clean", "ok"),
    ("push the code x branch and open the pr as ready for review", "Push the Codex branch and open the PR as ready for review.", "clean", "ok"),
    ("tail scale needs a login header for the owner", "Tailscale needs a login header for the owner.", "clean", "ok"),
    ("hi there just a quick note the meeting moved to three", "Hi there, just a quick note: the meeting moved to 3.", "email", "ok"),
    ("thanks for the review i will fix the two comments today", "Thanks for the review, I will fix the two comments today.", "email", "ok"),
    ("the parrot keet model loads in four seconds", "The Parakeet model loads in 4 seconds.", "clean", "ok"),
    ("switch the cleanup model to quen three eight b", "Switch the cleanup model to qwen3:8b.", "clean", "ok"),
    ("what is the status of the athena export", "What is the status of the Athena export?", "clean", "ok"),
    ("sixty six hundred rows were exported to click house", "6600 rows were exported to ClickHouse.", "clean", "ok"),
    ("clod please summarise the thread for the hand off", "Claude, please summarise the thread for the handoff.", "clean", "ok"),
    ("new line remember to stop the dev server when done", "\nRemember to stop the dev server when done.", "clean", "ok"),
    ("this one came back unchanged because the model timed out", "this one came back unchanged because the model timed out", "clean", "failed"),
    ("and this answer was far too long so the guard tripped", "and this answer was far too long so the guard tripped", "clean", "rejected"),
    ("raw mode keeps everything as heard um including fillers", "raw mode keeps everything as heard um including fillers", "raw", "off"),
    ("ctrl shift space dictates without cleanup", "ctrl shift space dictates without cleanup", "raw", "off"),
    ("zitra rano mergni ten pull request a pushni to na main", "Zítra ráno mergni ten pull request a pushni to na main.", "clean", "ok"),
    ("can you check the graph ana dashboard v pondeli rano", "Can you check the Grafana dashboard v pondělí ráno?", "clean", "ok"),
    ("the acer has four cores and seven gigabytes of ram", "The Acer has 4 cores and 7 GB of RAM.", "clean", "ok"),
    ("bazz ite updates itself every night", "Bazzite updates itself every night.", "clean", "ok"),
    ("open the t three code composer and paste the text", "Open the T3 Code composer and paste the text.", "clean", "ok"),
    ("let's pair on the sub agent limits tomorrow", "Let's pair on the subagent limits tomorrow.", "clean", "ok"),
    ("ten percent of the entries have a raw text that differs", "10% of the entries have a raw text that differs.", "clean", "ok"),
    ("copy this sentence to the clipboard and paste it", "Copy this sentence to the clipboard and paste it.", "clean", "ok"),
    ("dota two patch notes are out", "Dota 2 patch notes are out.", "clean", "ok"),
    ("the graph ana alert fired twice last night", "The Grafana alert fired twice last night.", "clean", "ok"),
]
DEVICES = ["desktop", "desktop", "laptop", "phone", "phone", "test"]
CLIENTS = {"desktop": "hark-linux/0.1.0", "laptop": "hark-linux/0.1.0", "phone": "hark-android/0.1.0", "test": "dev-page"}


def seed(state, count):
    rng = random.Random(7)
    modes = Modes(state / "modes.toml", vg.PROMPT)
    store = History(state, modes)
    now = datetime.now(timezone.utc)
    offsets = sorted((rng.uniform(0, 6 * 24 * 3600) for _ in range(count - 4)), reverse=True)
    offsets += [3 * 3600 + 200, 45 * 60, 90, 20]  # four today: hours, minutes, seconds, just now
    for index, seconds in enumerate(offsets):
        raw, text, mode, cleanup = SAMPLES[index % len(SAMPLES)]
        device = rng.choice(DEVICES)
        store.add({
            "time": (now - timedelta(seconds=seconds)).isoformat(timespec="seconds").replace("+00:00", "Z"),
            "device": device,
            "client": CLIENTS[device],
            "mode": mode,
            "audio_seconds": round(rng.uniform(1.5, 12.0), 2),
            "asr_seconds": round(rng.uniform(0.03, 0.2), 3),
            "cleanup_seconds": 0.0 if cleanup == "off" else round(rng.uniform(0.3, 0.9), 3),
            "cleanup": cleanup,
            "raw": raw,
            "text": text,
        })


def proxy_server(upstream_port, port, owner):
    """A loopback stand-in for Tailscale Serve: adds the owner login, fixes Origin as if HTTPS."""
    hop = {"connection", "keep-alive", "proxy-authenticate", "proxy-authorization", "te", "trailer",
           "transfer-encoding", "upgrade"}

    class Proxy(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def forward(self):
            length = int(self.headers.get("Content-Length") or 0)
            body = self.rfile.read(length) if length else None
            headers = {name: value for name, value in self.headers.items() if name.lower() not in hop}
            headers["Tailscale-User-Login"] = owner
            host = self.headers.get("Host", "")
            # The browser sends an http:// Origin on same-origin writes; Serve would see https://.
            if headers.get("Origin") == "http://" + host:
                headers["Origin"] = "https://" + host
            connection = http.client.HTTPConnection("127.0.0.1", upstream_port, timeout=120)
            try:
                connection.request(self.command, self.path, body, headers)
                response = connection.getresponse()
                payload = response.read()
                self.send_response(response.status)
                for name, value in response.getheaders():
                    if name.lower() not in hop and name.lower() != "content-length":
                        self.send_header(name, value)
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            finally:
                connection.close()

        do_GET = do_POST = do_DELETE = do_OPTIONS = forward

        def log_message(self, format, *args):
            logging.info("proxy %s %s", self.command, self.path.split("?", 1)[0])

    return ThreadingHTTPServer(("127.0.0.1", port), Proxy)


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--port", type=int, default=8780, help="proxy port the browser opens (default 8780)")
    parser.add_argument("--owner", default="dev@example.com", help="owner login the proxy asserts")
    parser.add_argument("--entries", type=int, default=40, help="synthetic history entries (default 40)")
    parser.add_argument("--quiet", action="store_true", help="hide request logging")
    args = parser.parse_args()
    logging.basicConfig(level=logging.WARNING if args.quiet else logging.INFO, format="%(levelname)s %(message)s")

    state = Path(tempfile.mkdtemp(prefix="hark-dev-page-"))
    # A SIGTERM (kill, systemd stop) must also remove the temp state, not just Ctrl-C.
    def stop(*_):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, stop)
    try:
        shutil.copy(HERE / "glossary.txt", state / "glossary.txt")
        (state / "modes.toml").write_text(
            (HERE / "modes.toml").read_text(encoding="utf-8")
            + '\n[email]\ndescription = "Polite email"\nprompt = "Rewrite the dictation as a polite email."\n',
            encoding="utf-8")
        seed(state, max(args.entries, 4))

        def transcribe(data):
            return data.decode(errors="replace").strip()[:500], 1.0, None

        def cleanup(raw, glossary):
            return raw[:1].upper() + raw[1:] + ".", "ok"

        gateway = vg.create_server(transcribe, TOKEN, state, cleanup, port=0, owner=args.owner)
        threading.Thread(target=gateway.serve_forever, daemon=True).start()
        proxy = proxy_server(gateway.server_address[1], args.port, args.owner)
        print(f"hark dev page: http://127.0.0.1:{proxy.server_address[1]}/  (state {state}, Ctrl-C stops)", flush=True)
        try:
            proxy.serve_forever()
        except KeyboardInterrupt:
            pass
        finally:
            proxy.server_close()
            gateway.shutdown()
            gateway.server_close()
    finally:
        shutil.rmtree(state, ignore_errors=True)


if __name__ == "__main__":
    main()
