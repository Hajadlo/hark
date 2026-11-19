"""Contract tests using synthetic dictations and local HTTP servers."""

import concurrent.futures
import http.client
import json
import sqlite3
from urllib.parse import urlencode

import pytest

import voice_gateway as vg
from gateway_state import History, Modes
from test_voice_gateway import AUDIO, TOKEN, call, chat_server, serving, upload

BEARER = {"Authorization": f"Bearer {TOKEN}"}
OWNER = {"Tailscale-User-Login": "OWNER@EXAMPLE.COM", "X-Hark-Web": "1"}


def api(base, route, data=None, headers=BEARER, method=None):
    status, body = call(base + route, data, headers, method)
    return status, json.loads(body)


def record(raw="clod coat", text="Claude Code", cleanup="ok", mode="clean", device="desktop"):
    return dict(time="2026-10-11T16:20:31Z", device=device, client="test", mode=mode,
                audio_seconds=2.5, asr_seconds=0.051, cleanup_seconds=0.412,
                cleanup=cleanup, raw=raw, text=text)


def history(state):
    return History(state, Modes(state / "modes.toml", vg.PROMPT))


def test_modes_reload_order_and_invalid_tables(tmp_path, caplog):
    with serving(vg.create_server(lambda _: ("hello", 1, None), TOKEN, tmp_path, port=0)) as base:
        status, result = api(base, "/v1/models")
        assert status == 200
        assert result == {"object": "list", "data": [
            {"id": "clean", "object": "model", "owned_by": "hark", "description": "Punctuation, numbers, no filler words", "cleanup": True},
            {"id": "raw", "object": "model", "owned_by": "hark", "description": "As heard", "cleanup": False},
        ]}
        path = tmp_path / "modes.toml"
        path.write_text('[email]\ndescription="Polite email"\nprompt="Be polite"\ndrift_guard=false\n'
                        '[verbatim]\ndescription="No cleanup"\n["BAD!"]\nprompt="skip"\n'
                        '[wrong-type]\nprompt=17\n')
        models = api(base, "/v1/models")[1]["data"]
        assert [m["id"] for m in models] == ["email", "verbatim", "clean", "raw"]
        assert [m["cleanup"] for m in models] == [True, False, True, False]
        assert json.loads(upload(base, {"file": AUDIO, "model": "verbatim"})[1])["cleanup"] == "off"
        path.write_text("broken = [")
        assert api(base, "/v1/models")[1]["data"] == models
        assert json.loads(upload(base, {"file": AUDIO, "model": "verbatim"})[1])["mode"] == "verbatim"
        assert "last valid version" in caplog.text and "Skipping invalid mode" in caplog.text
        path.unlink()
        assert [m["id"] for m in api(base, "/v1/models")[1]["data"]] == ["clean", "raw"]


def test_mode_prompts_and_drift_guard(tmp_path, monkeypatch):
    path = tmp_path / "modes.toml"
    path.write_text('[email]\nprompt="Write a polite email."\ndrift_guard=false\n')
    (tmp_path / "glossary.txt").write_text("Claude Code\n")
    long_reply = "Hello! " * 20
    with chat_server(long_reply) as (url, requests):
        monkeypatch.setattr(vg, "LLM_URL", url)
        with serving(vg.create_server(lambda _: ("hello", 1, None), TOKEN, tmp_path, port=0)) as base:
            result = json.loads(upload(base, {"file": AUDIO, "model": "email"})[1])
            assert result["text"] == long_reply.strip() and result["cleanup"] == "ok"
            assert requests[-1]["messages"][0]["content"].startswith("Write a polite email.")
            assert "Claude Code" in requests[-1]["messages"][0]["content"]
            path.write_text('[email]\nprompt="Write a polite email."\n')
            assert json.loads(upload(base, {"file": AUDIO, "model": "email"})[1])["cleanup"] == "rejected"
            assert json.loads(upload(base, {"file": AUDIO, "model": "whisper-1"})[1])["mode"] == "clean"
    path.write_text("broken = [")
    with serving(vg.create_server(lambda _: ("hello", 1, None), TOKEN, tmp_path, port=0)) as base:
        assert [m["id"] for m in api(base, "/v1/models")[1]["data"]] == ["clean", "raw"]


def test_history_search_paging_filters_and_metadata(tmp_path):
    store = history(tmp_path)
    first = store.add(record(raw="open clod coat", text="Open Claude Code."))
    second = store.add(record(raw="code only", text="Code only", mode="raw", device="laptop"))
    third = store.add(record(raw="Claude here; code there", text="same", device="test"))
    with serving(vg.create_server(lambda _: ("   ", 1, None), TOKEN, tmp_path, port=0)) as base:
        result = api(base, "/v1/history?" + urlencode({"q": "clau cod"}))[1]
        assert [r["id"] for r in result["data"]] == [third, first]
        assert result["next_before"] is None
        for query in ['"Claude" *code', "Claude-code", "Claude:code", "*", '"', "-", ":", "NEAR", "NEAR(Claude Code)"]:
            status, result = api(base, "/v1/history?" + urlencode({"q": query}))
            assert status == 200
            if "NEAR" in query:
                assert result["data"] == []
        result = api(base, "/v1/history?limit=2")[1]
        assert [r["id"] for r in result["data"]] == [third, second]
        assert result["next_before"] == second
        result = api(base, f"/v1/history?limit=2&before={second}")[1]
        assert [r["id"] for r in result["data"]] == [first] and result["next_before"] is None
        assert api(base, "/v1/history?before=1")[1] == {"data": [], "next_before": None}
        assert [r["id"] for r in api(base, "/v1/history?device=laptop&mode=raw")[1]["data"]] == [second]
        assert api(base, "/v1/history?device=desktop&mode=raw")[1]["data"] == []
        result = json.loads(upload(base, {"file": AUDIO, "model": "raw"})[1])
        assert result == {"text": "", "id": None, "mode": "raw", "cleanup": "off", "language": None}
        assert len(api(base, "/v1/history")[1]["data"]) == 3


@pytest.mark.parametrize("query", ["limit=0", "limit=201", "limit=-1", "limit=1.5", "limit=", "limit=1&limit=2",
                                      "before=0", "before=no", "before=-1", "before=9999999999999999999999999",
                                      "device=", "device=A", "mode=bad!", "q=a&q=b"])
def test_bad_history_parameters(tmp_path, query):
    with serving(vg.create_server(lambda _: ("", 0, None), TOKEN, tmp_path, port=0)) as base:
        status, body = api(base, "/v1/history?" + query)
        assert status == 400 and body["error"]["message"]


def test_import_once_transaction_and_rename(tmp_path):
    old = dict(timestamp="2026-10-11T18:20:31+02:00", pipeline="whisper-1", client="old",
               audio_seconds=2.5, asr_seconds=0.1, cleanup_seconds=0.2, cleanup="ok", raw="clod coat", text="Claude Code")
    path = tmp_path / "history.jsonl"
    path.write_text(json.dumps(old) + "\n" + json.dumps(dict(old, pipeline="raw")) + "\n")
    for _ in range(2):
        with serving(vg.create_server(lambda _: ("", 0, None), TOKEN, tmp_path, port=0)) as base:
            rows = api(base, "/v1/history")[1]["data"]
            assert len(rows) == 2
            assert all(row['language'] is None for row in rows)
            assert [r["mode"] for r in rows] == ["raw", "clean"]
            assert rows[0]["time"] == "2026-10-11T16:20:31Z" and rows[0]["device"] == "unknown"
            assert rows[0]["client"] == "old"
            assert api(base, "/v1/history?q=clau+cod")[1]["data"] == rows
    assert not path.exists() and (tmp_path / "history.jsonl.imported").read_text().startswith('{"timestamp"')
    # Simulate the window after commit but before the original rename.
    path.write_text(json.dumps(old) + "\n")
    assert len(history(tmp_path).search({})["data"]) == 2
    assert not path.exists()


def test_import_failure_rolls_back(tmp_path):
    old = dict(timestamp="2026-10-11T16:20:31Z", pipeline="clean", **{k: v for k, v in record().items() if k not in {"time", "device", "mode"}})
    path = tmp_path / "history.jsonl"
    path.write_text(json.dumps(old) + "\ninvalid\n")
    with pytest.raises(ValueError):
        history(tmp_path)
    with sqlite3.connect(tmp_path / "history.sqlite3") as db:
        assert db.execute("SELECT count(*) FROM history").fetchone()[0] == 0
    assert path.exists() and not (tmp_path / "history.jsonl.imported").exists()
    path.write_text(json.dumps(old) + "\n")
    assert len(history(tmp_path).search({})["data"]) == 1


def test_corrections_grouping_skips_glossary_and_order(tmp_path):
    store = history(tmp_path)
    for _ in range(3):
        last = store.add(record(raw="open clod coat today", text="Open Claude Code today."))
    for raw, final in [("hello world", "Hello world."), ("um open code", "Open code."),
                       ("sixty six", "66"), ("hello", "Hello there"),
                       ("a b c d e", "v w x y z")]:
        store.add(record(raw=raw, text=final))
    store.add(record(raw="clod coat", text="Claude Code", cleanup="failed"))
    store.add(record(raw="clod coat", text="Claude Code", cleanup="off"))
    other = store.add(record(raw="rusty", text="Rust"))
    # The contract's case-insensitive word algorithm treats shared 'code' as equal.
    store.add(record(raw="clod code", text="Claude Code"))
    (tmp_path / "glossary.txt").write_text("# names\nclaude code\n")
    with serving(vg.create_server(lambda _: ("", 0, None), TOKEN, tmp_path, port=0)) as base:
        result = api(base, "/v1/corrections")[1]
        assert result["entries_scanned"] == 10
        assert result["data"][0] == {"raw": "clod coat", "final": "Claude Code", "count": 3,
                                     "last_id": last, "last_time": "2026-10-11T16:20:31Z", "in_glossary": True}
        assert [(r["raw"], r["final"]) for r in result["data"][1:]] == [("clod", "Claude"), ("rusty", "Rust")]
        assert result["data"][-1]["last_id"] == other and not result["data"][-1]["in_glossary"]
        assert len(api(base, "/v1/corrections?min_count=2&limit=1")[1]["data"]) == 1
        assert api(base, "/v1/corrections?min_count=4")[1]["data"] == []
        (tmp_path / "glossary.txt").write_text("Rust\n")
        assert api(base, "/v1/corrections")[1]["data"][-1]["in_glossary"]
        for query in ("limit=0", "limit=501", "min_count=0", "min_count=x", "limit=1&limit=2"):
            assert api(base, "/v1/corrections?" + query)[0] == 400


def test_glossary_add_duplicate_remove_and_comments(tmp_path):
    path = tmp_path / "glossary.txt"
    original = "# terms\n\nPR\n  # more names\nClaude Code\nclaude code\n\n"
    path.write_text(original)
    with serving(vg.create_server(lambda _: ("", 0, None), TOKEN, tmp_path, port=0)) as base:
        assert api(base, "/v1/glossary")[1]["terms"] == ["PR", "Claude Code", "claude code"]
        headers = dict(BEARER, **{"Content-Type": "application/json"})
        result = api(base, "/v1/glossary", b'{"term":"  Rust  "}', headers)[1]
        assert result == {"terms": ["PR", "Claude Code", "claude code", "Rust"], "added": True}
        assert path.read_text() == original + "Rust\n"
        assert api(base, "/v1/glossary", b'{"term":"rUsT"}', headers)[1]["added"] is False
        result = api(base, "/v1/glossary?term=CLAUDE+CODE", method="DELETE")[1]
        assert result == {"terms": ["PR", "Rust"], "removed": True}
        assert path.read_text() == "# terms\n\nPR\n  # more names\n\nRust\n"
        assert api(base, "/v1/glossary?term=missing", method="DELETE")[1]["removed"] is False
        for term in ("x", "x" * 64):
            assert api(base, "/v1/glossary", json.dumps({"term": term}).encode(), headers)[1]["added"]
    assert not list(tmp_path.glob(".glossary-*"))


def test_corrections_shared_word_contract_example(tmp_path):
    store = history(tmp_path)
    for _ in range(3):
        last = store.add(record(raw="clod code", text="Claude Code"))
    (tmp_path / "glossary.txt").write_text("Claude\n")
    with serving(vg.create_server(lambda _: ("", 0, None), TOKEN, tmp_path, port=0)) as base:
        assert api(base, "/v1/corrections")[1] == {"data": [
            {"raw": "clod", "final": "Claude", "count": 3, "last_id": last,
             "last_time": "2026-10-11T16:20:31Z", "in_glossary": True}
        ], "entries_scanned": 3}


@pytest.mark.parametrize("term", ["", "   ", "x" * 65, "a,b", " # hi", "a\nb", "a\rb", "a\x00b", "a\tb", "a\x7fb", "a\u0085b", "a\u2028b", 7, None])
def test_glossary_term_rules(tmp_path, term):
    with serving(vg.create_server(lambda _: ("", 0, None), TOKEN, tmp_path, port=0)) as base:
        headers = dict(BEARER, **{"Content-Type": "application/json"})
        assert api(base, "/v1/glossary", json.dumps({"term": term}).encode(), headers)[0] == 400
        if isinstance(term, str):
            assert api(base, "/v1/glossary?" + urlencode({"term": term}), method="DELETE")[0] == 400


def test_auth_each_route_and_owner_guards(tmp_path):
    routes = [("GET", "/v1/models", None), ("GET", "/v1/history", None),
              ("GET", "/v1/corrections", None), ("GET", "/v1/glossary", None),
              ("POST", "/v1/glossary", b'{"term":"Rust"}'), ("DELETE", "/v1/glossary?term=Rust", None)]
    with serving(vg.create_server(lambda _: ("hello", 1, None), TOKEN, tmp_path, port=0)) as base:
        for method, route, body in routes:
            for auth in (BEARER, OWNER):
                headers = dict(auth, **({"Content-Type": "application/json"} if body else {}))
                assert api(base, route, body, headers, method)[0] == 200
            for auth in ({}, {"Authorization": "Bearer wrong"}, dict(OWNER, **{"Tailscale-User-Login": "wrong"}),
                         {"Tailscale-User-Login": "owner@example.com"}, dict(OWNER, Origin="https://foreign.example")):
                headers = dict(auth, **({"Content-Type": "application/json"} if body else {}))
                assert api(base, route, body, headers, method)[0] == 401
        host = base.removeprefix("http://")
        assert api(base, "/v1/history", headers=dict(OWNER, Origin="https://" + host))[0] == 200
        assert api(base, "/v1/history", headers=dict(OWNER, Origin="http://" + host))[0] == 401
        assert api(base, "/v1/glossary", b'{"term":"Rust"}', OWNER)[0] == 401
        assert api(base, "/v1/glossary", b'{"term":"Rust"}', dict(OWNER, **{"Content-Type": "text/plain"}))[0] == 401
        assert upload(base, {"file": AUDIO, "model": "raw"}, token=None, extra_headers=OWNER)[0] == 401
        assert upload(base, {"file": AUDIO, "model": "raw"})[0] == 200
        assert call(base + "/", headers=BEARER)[0] == 403
        assert call(base + "/", headers={"Tailscale-User-Login": "wrong"})[0] == 403
        request = vg.urllib.request.Request(base + "/", headers={"Tailscale-User-Login": "OWNER@EXAMPLE.COM"})
        with vg.urllib.request.urlopen(request) as response:
            page = response.read()
            assert response.status == 200 and b"<title>hark</title>" in page and b'"X-Hark-Web": "1"' in page
            assert response.headers["Content-Security-Policy"] == vg.CSP
            assert response.headers["Content-Type"] == "text/html; charset=utf-8"
            assert response.headers["Cache-Control"] == "no-store"
            assert response.headers["Referrer-Policy"] == "no-referrer"
            assert response.headers["X-Content-Type-Options"] == "nosniff"
        status, body = api(base, "/v1/history", headers=OWNER, method="OPTIONS")
        assert status == 403 and body["error"]["message"]
        assert api(base, "/v1/no-such-route")[0] == 404


def test_custom_owner_and_token_minimum(tmp_path):
    with pytest.raises(ValueError, match="32"):
        vg.create_server(lambda _: ("", 0, None), "short", tmp_path, port=0)
    with serving(vg.create_server(lambda _: ("", 0, None), TOKEN, tmp_path, port=0, owner="me@example.com")) as base:
        assert api(base, "/v1/models", headers=OWNER)[0] == 401
        assert api(base, "/v1/models", headers={"Tailscale-User-Login": "ME@example.com", "X-Hark-Web": "1"})[0] == 200


def test_device_validation_and_private_access_logs(tmp_path, caplog):
    import logging
    caplog.set_level(logging.INFO)
    with serving(vg.create_server(lambda _: ("hello", 1, None), TOKEN, tmp_path, port=0)) as base:
        for device, expected in [("PHONE", "phone"), ("bad device", "unknown"), ("x" * 33, "unknown"), ("", "unknown")]:
            entry = json.loads(upload(base, {"file": AUDIO, "model": "raw"}, extra_headers={"X-Hark-Device": device})[1])
            row = api(base, "/v1/history?limit=1")[1]["data"][0]
            assert row["id"] == entry["id"] and row["device"] == expected
        assert api(base, "/v1/history?q=secret-search-words")[0] == 200
        assert api(base, "/v1/glossary?term=secret-glossary-words", method="DELETE")[0] == 200
    assert "secret-search-words" not in caplog.text and "secret-glossary-words" not in caplog.text


def test_threaded_history_and_glossary_writes(tmp_path):
    with serving(vg.create_server(lambda data: (data.decode(), 1, None), TOKEN, tmp_path, port=0)) as base:
        def submit(i):
            status, body = upload(base, {"file": f"recording {i}", "model": "raw"},
                                  extra_headers={"X-Hark-Device": "TEST", "User-Agent": "c" * 100})
            assert status == 200
            status, result = api(base, "/v1/glossary", json.dumps({"term": f"term {i}"}).encode(),
                                 dict(BEARER, **{"Content-Type": "application/json"}))
            assert status == 200 and result["added"]
            return json.loads(body)["id"]
        with concurrent.futures.ThreadPoolExecutor(max_workers=6) as pool:
            ids = list(pool.map(submit, range(12)))
        assert len(set(ids)) == 12
        assert len(api(base, "/v1/glossary")[1]["terms"]) == 12
        rows = api(base, "/v1/history?q=record")[1]["data"]
        assert len(rows) == 12 and all(r["device"] == "test" and len(r["client"]) == 80 for r in rows)
    assert {p.name for p in tmp_path.iterdir()} <= {"history.sqlite3", "history.sqlite3-wal", "history.sqlite3-shm", "glossary.txt"}


def test_upload_limits_bad_length_and_queue(tmp_path, monkeypatch):
    with serving(vg.create_server(lambda _: ("hello", 1, None), TOKEN, tmp_path, port=0)) as base:
        host = base.removeprefix("http://")
        for length, status in [(None, 411), ("-1", 400), ("no", 400), (str(vg.MAX_BYTES + 1), 413)]:
            conn = http.client.HTTPConnection(host)
            headers = dict(BEARER)
            if length is not None:
                headers["Content-Length"] = length
            conn.putrequest("POST", "/v1/audio/transcriptions")
            for name, value in headers.items():
                conn.putheader(name, value)
            conn.endheaders()
            response = conn.getresponse()
            assert response.status == status and json.loads(response.read())["error"]["message"]
            conn.close()
    import threading
    entered, release = threading.Event(), threading.Event()

    def blocked(_):
        entered.set()
        assert release.wait(5)
        return "hello", 1, None

    monkeypatch.setattr(vg, "QUEUE_TIMEOUT", 0.05)
    with serving(vg.create_server(blocked, TOKEN, tmp_path, port=0)) as base:
        with concurrent.futures.ThreadPoolExecutor() as pool:
            first = pool.submit(upload, base, {"file": AUDIO, "model": "raw"})
            try:
                assert entered.wait(2)
                assert upload(base, {"file": AUDIO, "model": "raw"})[0] == 503
            finally:
                release.set()
            assert first.result()[0] == 200
