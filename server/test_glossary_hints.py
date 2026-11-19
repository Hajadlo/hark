"""Heard-as glossary hints and the optional hinted English Whisper pass."""
import json
import sys
from types import SimpleNamespace

import voice_gateway as vg
from gateway_state import read_glossary, read_glossary_hints
from recognition import Recognizer, english_initial_prompt, glossary_respellings
from test_gateway_api import BEARER, TOKEN, api, serving
from test_recognition import FakeModel, speech
from test_voice_gateway import chat_server


def write(tmp_path, text):
    path = tmp_path / "glossary.txt"
    path.write_text(text, encoding="utf-8")
    return path


def test_hint_lines_keep_terms_and_parse_heard_as(tmp_path):
    path = write(tmp_path, "# names\nskald: sculpt, kalt\nMork: Morg\nRe:Zero\nClaude Code\nU Elektrárny\nBad: , \n")
    assert read_glossary(path) == ["skald", "Mork", "Re:Zero", "Claude Code", "U Elektrárny", "Bad"]
    assert read_glossary_hints(path) == {"skald": ["sculpt", "kalt"], "Mork": ["Morg"]}


def test_api_lists_hints_and_delete_removes_a_hinted_line(tmp_path):
    path = write(tmp_path, "skald: sculpt, kalt\nPR\n")
    with serving(vg.create_server(lambda _: ("", 0, None), TOKEN, tmp_path, port=0)) as base:
        assert api(base, "/v1/glossary")[1] == {"terms": ["skald", "PR"], "hints": {"skald": ["sculpt", "kalt"]}}
        headers = dict(BEARER, **{"Content-Type": "application/json"})
        assert api(base, "/v1/glossary", b'{"term":"SKALD"}', headers)[1]["added"] is False
        assert api(base, "/v1/glossary", b'{"term":"a: b"}', headers)[0] == 400
        assert api(base, "/v1/glossary", b'{"term":"Re:Zero"}', headers)[1]["added"] is True
        assert api(base, "/v1/glossary?term=Re%3AZero", method="DELETE")[1]["removed"] is True
        assert api(base, "/v1/glossary?term=skald", method="DELETE")[1] == {"terms": ["PR"], "removed": True}
    assert path.read_text() == "PR\n"


def test_cleanup_prompt_names_the_mishearings():
    with chat_server("Ask skald.") as (url, requests):
        assert vg.clean("Ask sculpt.", ["skald"], url, "m", hints={"skald": ["sculpt", "kalt"]}) == ("Ask skald.", "ok")
    system = requests[0]["messages"][0]["content"]
    assert '"sculpt" or "kalt" -> skald' in system
    with chat_server("Ask skald.") as (url, requests):
        vg.clean("Ask skald.", ["skald"], url, "m")
    assert "mishears" not in requests[0]["messages"][0]["content"]


def test_english_prompt_budget_and_empty():
    tokenize = lambda text: text.split()
    prompt = english_initial_prompt(["Term" + str(i) for i in range(500)], tokenize)
    assert len(tokenize(" " + prompt)) <= 223 and "Term499" in prompt and "Term0," not in prompt
    assert english_initial_prompt([]) is None
    assert english_initial_prompt(["skald", "Mork"]) == "Names and terms: skald, Mork."


def test_hint_only_respells_glossary_terms():
    terms = ["skald", "Mork", "Kubernetes", "Claude Code"]
    r = lambda cand, hinted: glossary_respellings(cand, hinted, terms)
    assert r("ask sculpt about it", "ask skald about it") == "ask skald about it"
    assert r("comments from Morg.", "comments from Mork.") == "comments from Mork."
    assert r("the bad boom report", "the Kubernetes report") == "the bad boom report"  # merges: cleanup's job
    # Meaning changes, additions, drops and echoes keep the candidate's words.
    assert r("turn off the lights", "turn on the lights") == "turn off the lights"
    assert r("ask sculpt", "ask skald and Mork too") == "ask sculpt"
    assert r("ask sculpt about it today", "ask") == "ask sculpt about it today"
    assert r("hello", "skald") == "hello"
    assert r("hello", "Names and terms: skald.") == "hello"
    # Rounds 2-3: no merges (they can swallow "not"); punctuation stays the candidate's.
    assert r("do not work today", "do Mork today") == "do not work today"
    assert r("do not bet boom today", "do Kubernetes today") == "do not bet boom today"
    assert r("ask Morg, please", "ask Mork. please") == "ask Mork, please"


def english_model(hinted_text):
    model = FakeModel("en")
    def run(pcm, **kw):
        model.calls.append(kw)
        hinted = bool(kw.get("family", {}).get("initial_prompt"))
        return SimpleNamespace(text=hinted_text if hinted else "ask sculpt about it", language="en",
                               tokens=[SimpleNamespace(id=10, p=.9)])
    model.run = run
    return model


def test_english_hint_runs_after_the_english_choice():
    model = english_model("ask skald about it")
    rec = Recognizer(model, "en,cs", lambda **kw: kw)
    assert rec.run(speech(), english_prompt="Names and terms: skald.", english_terms=["skald"]) == ("ask skald about it", "en")
    assert not model.calls[0].get("family", {}).get("initial_prompt")
    assert model.calls[-1]["language"] == "en" and model.calls[-1]["family"]["initial_prompt"] == "Names and terms: skald."


def test_english_hint_off_or_implausible_keeps_candidate():
    model = english_model("ask skald about it")
    assert Recognizer(model, "en,cs", lambda **kw: kw).run(speech()) == ("ask sculpt about it", "en")
    assert len(model.calls) == 1
    model = english_model("Names and terms: skald.")
    assert Recognizer(model, "en,cs", lambda **kw: kw).run(speech(), english_prompt="Names and terms: skald.",
                                                         english_terms=["skald"])[0] == "ask sculpt about it"


def test_english_hint_failure_is_optional(caplog):
    model = english_model("x")
    original = model.run
    def run(pcm, **kw):
        if kw.get("family", {}).get("initial_prompt"):
            raise RuntimeError("private text")
        return original(pcm, **kw)
    model.run = run
    assert Recognizer(model, "en,cs", lambda **kw: kw).run(speech(), english_prompt="p")[0] == "ask sculpt about it"
    assert any("Optional English prompt failed" in r.message for r in caplog.records)
    assert all("private text" not in r.message for r in caplog.records)


def test_gateway_setting_gates_the_english_prompt(monkeypatch, tmp_path):
    model = english_model("ask skald about it")
    monkeypatch.setitem(sys.modules, "av", SimpleNamespace(error=SimpleNamespace(FFmpegError=ValueError)))
    monkeypatch.setitem(sys.modules, "transcribe_cpp", SimpleNamespace(Model=lambda path: model, WhisperRunOptions=lambda **kw: kw))
    monkeypatch.setattr(vg, "STATE", tmp_path)
    monkeypatch.setattr(vg, "decode_audio", lambda av, np, data: speech())
    write(tmp_path, "skald: sculpt\n")
    monkeypatch.setattr(vg, "ENGLISH_HINT", False)
    assert vg.load_transcriber()(b"audio")[0] == "ask sculpt about it"
    monkeypatch.setattr(vg, "ENGLISH_HINT", True)
    assert vg.load_transcriber()(b"audio")[0] == "ask skald about it"
    assert model.calls[-1]["family"]["initial_prompt"] == "Names and terms: skald."
