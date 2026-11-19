from array import array
import math
import random
import sqlite3
from types import SimpleNamespace
from pathlib import Path
import tomllib

import pytest
import voice_gateway as vg
from gateway_state import History, Modes
from recognition import Recognizer, has_speech, speech_windows, speech_evidence
from test_voice_gateway import AUDIO, TOKEN, serving, upload
from test_gateway_api import api


class FakeModel:
    arch = 'whisper'

    def __init__(self, detected='en', multilingual=True):
        self.capabilities = SimpleNamespace(supports_language_detect=multilingual, languages=('en', 'cs', 'pl', 'de'))
        self.detected, self.calls = detected, []

    def session(self):
        return self

    def run(self, pcm, **options):
        self.calls.append(options)
        return SimpleNamespace(text='hello', language=options.get('language') or self.detected)


def speech(amplitude=0.004):
    return array('f', (amplitude * (0.5+0.5*math.sin(2*math.pi*3*i/16000)) *
                      (math.sin(2*math.pi*160*i/16000)+0.3*math.sin(2*math.pi*320*i/16000)) for i in range(16000)))


@pytest.mark.parametrize('detected,expected', [('en','en'),('cs','cs'),('pl','cs'),('sk','cs'),('ru','cs'),('de','en'),('', 'en')])
def test_language_policy_uses_decoder_hint(detected, expected):
    model = FakeModel(detected)
    assert Recognizer(model, 'en,cs').run(speech()) == ('hello', expected)
    assert len(model.calls) == (1 if detected in {'en','cs'} else 2)
    if len(model.calls)>1:
        assert model.calls[-1]['language'] == expected


def test_language_configuration_and_english_only():
    english = FakeModel(multilingual=False)
    assert Recognizer(english, 'not-a-language').run(speech()) == ('hello', None)
    model = FakeModel('de')
    assert Recognizer(model, 'cs').run(speech()) == ('hello', 'cs')
    assert model.calls == [{'language':'cs', 'timestamps':'none'}]
    for value in ['', 'en,xx']:
        with pytest.raises(ValueError):
            Recognizer(FakeModel(), value)


def test_warmup_bypasses_speech_gate():
    model = FakeModel()
    recognizer = Recognizer(model,'en,cs')
    assert recognizer.run(array('f',[0])*16000) == ('',None)
    assert not model.calls
    assert recognizer.run(array('f',[0])*16000,warmup=True) == ('hello','en')
    assert len(model.calls) == 1


def test_gate_rejects_silence_noise_click_and_breath_keeps_quiet_speech():
    rng = random.Random(42)
    zeros = array('f', [0])*32000
    noise = array('f', (rng.gauss(0, 0.00015) for _ in zeros))
    click = zeros[:]
    click[8000] = 0.8
    breath = array('f', (rng.gauss(0, 0.01)*math.sin(math.pi*i/16000)**2 for i in range(16000)))
    for pcm in [zeros, click]:
        assert not has_speech(pcm)
        model = FakeModel()
        assert Recognizer(model, 'en,cs').run(pcm) == ('', None)
        assert not model.calls
    assert speech_evidence(noise) == (True,False)
    assert speech_evidence(breath) == (True,False)
    assert has_speech(speech(0.0015))


@pytest.mark.parametrize('language', ['en','cs',None])
def test_language_response_and_history(tmp_path, language):
    with serving(vg.create_server(lambda _: ('hello', 1, language), TOKEN, tmp_path, port=0)) as base:
        import json
        result = json.loads(upload(base, {'file':AUDIO, 'model':'raw'})[1])
        assert result['language'] == language
        assert api(base, '/v1/history')[1]['data'][0]['language'] == language


def test_old_database_migration_preserves_entries_fts_and_import(tmp_path):
    path = tmp_path/'history.sqlite3'
    with sqlite3.connect(path) as db:
        db.execute('CREATE TABLE history (id INTEGER PRIMARY KEY AUTOINCREMENT, time TEXT NOT NULL, device TEXT NOT NULL, client TEXT NOT NULL, mode TEXT NOT NULL, audio_seconds REAL NOT NULL, asr_seconds REAL NOT NULL, cleanup_seconds REAL NOT NULL, cleanup TEXT NOT NULL, raw TEXT NOT NULL, text TEXT NOT NULL)')
        db.execute("INSERT INTO history VALUES (1,'2026-10-11T00:00:00Z','test','test','raw',1,0,0,'off','hello','Hello')")
        db.execute("CREATE VIRTUAL TABLE history_fts USING fts5(raw,text,content='history',content_rowid='id')")
        db.execute("INSERT INTO history_fts(history_fts) VALUES ('rebuild')")
    for _ in range(2):
        store = History(tmp_path, Modes(tmp_path/'modes.toml',vg.PROMPT))
        row = store.search({'q':['hello']})['data'][0]
        assert row['id'] == 1 and row['text'] == 'Hello' and row['language'] is None
    with sqlite3.connect(path) as db:
        assert [r[1] for r in db.execute('PRAGMA table_info(history)')].count('language') == 1


def test_prompts_identical_and_long_window_options():
    assert tomllib.loads(Path(vg.__file__).with_name('modes.toml').read_text())['clean']['prompt'] == vg.PROMPT
    model = FakeModel()
    options = lambda **values: values
    Recognizer(model,'en,cs',options).run(speech()*31)
    assert model.calls[0]['timestamps'] == 'segment'
    assert model.calls[0]['family'] == {'condition_on_prev_tokens':False}


def test_long_windows_cover_audio_and_redetect_language():
    pcm = speech()*61
    windows = list(speech_windows(pcm))
    assert sum(map(len, windows)) == len(pcm)
    assert all(len(window) <= 30*16000 for window in windows)
    assert array('f', (sample for window in windows for sample in window)) == pcm
    model = FakeModel()
    def run(window, **options):
        model.calls.append(options)
        language = 'cs' if len(model.calls)==2 else 'en'
        return SimpleNamespace(text=language,language=language)
    model.run = run
    text, language = Recognizer(model,'en,cs',lambda **v:v).run(pcm)
    assert text == 'en cs en' and language == 'en'
    assert len(model.calls) == 3
    assert [c['language'] for c in model.calls] == [None,None,None]


@pytest.mark.parametrize('text', ['you','Thank you.','Thanks for watching!','Titulky vytvořil JohnyX','Děkuji za pozornost.','.'])
def test_whisper_noise_phrases_only_when_evidence_weak(text):
    import numpy as np
    rng = np.random.default_rng(42)
    weak = rng.normal(0,.01,16000).astype(np.float32)
    model = FakeModel()
    model.run = lambda pcm,**kw: SimpleNamespace(text=text,language='en')
    recognizer = Recognizer(model,'en,cs',lambda **kw:kw)
    assert recognizer.run(weak) == ('',None)
    assert recognizer.run(speech()) == (text,'en')
    # Ordinary short words are never discarded just for lacking pitch.
    model.run = lambda pcm,**kw: SimpleNamespace(text='Stop.',language='en')
    assert recognizer.run(weak) == ('Stop.','en')


def test_trimmed_short_speech_does_not_become_its_own_noise_floor():
    import numpy as np
    time = np.arange(1600)/16000
    pcm = (.02*np.sin(2*np.pi*170*time)+.01*np.sin(2*np.pi*340*time)).astype(np.float32)
    assert has_speech(pcm)
    assert speech_evidence(pcm)[1]
    assert has_speech(pcm*.005)  # Quiet, trimmed word: RMS below the old capped floor.


@pytest.mark.parametrize('seconds',[30.1,31,59.9])
def test_every_non_silent_sample_reaches_asr_even_in_a_short_tail(seconds):
    import numpy as np
    pcm = np.full(round(seconds*16000),.01,dtype=np.float32)
    windows = list(speech_windows(pcm))
    assert sum(map(len,windows)) == len(pcm)
    assert min(map(len,windows)) >= 16000
    model = FakeModel()
    sent = []
    def run(window,**kw):
        sent.append(window.copy())
        return SimpleNamespace(text='end',language='en')
    model.run = run
    assert Recognizer(model,'en,cs',lambda **kw:kw).run(pcm)[0]
    assert np.array_equal(np.concatenate(sent),pcm)


def test_concurrent_migration_preserves_old_rows(tmp_path):
    from concurrent.futures import ThreadPoolExecutor
    import threading
    with sqlite3.connect(tmp_path/'history.sqlite3') as db:
        db.execute('CREATE TABLE history (id INTEGER PRIMARY KEY AUTOINCREMENT, time TEXT NOT NULL, device TEXT NOT NULL, client TEXT NOT NULL, mode TEXT NOT NULL, audio_seconds REAL NOT NULL, asr_seconds REAL NOT NULL, cleanup_seconds REAL NOT NULL, cleanup TEXT NOT NULL, raw TEXT NOT NULL, text TEXT NOT NULL)')
        db.execute("INSERT INTO history VALUES (1,'2026-10-11T00:00:00Z','test','test','raw',1,0,0,'off','hello','Hello')")
    barrier = threading.Barrier(2)
    def open_history(_):
        barrier.wait()
        return History(tmp_path,Modes(tmp_path/'modes.toml',vg.PROMPT))
    with ThreadPoolExecutor(max_workers=2) as pool:
        stores = list(pool.map(open_history,range(2)))
    assert all(store.search({})['data'][0]['text']=='Hello' for store in stores)


def test_hum_is_weak_evidence_and_quiet_window_is_not_skipped():
    import numpy as np
    hum = np.concatenate([np.zeros(16000),.01*np.sin(2*np.pi*200*np.arange(32000)/16000),np.zeros(16000)]).astype(np.float32)
    assert speech_evidence(hum) == (True,False)
    pcm = np.full(481600,.00006,dtype=np.float32)
    model = FakeModel()
    sent = []
    def run(window,**kw):
        sent.append(len(window))
        return SimpleNamespace(text='yes',language='en')
    model.run = run
    assert Recognizer(model,'en,cs',lambda **v:v).run(pcm)[0]
    assert sum(sent) == len(pcm)
