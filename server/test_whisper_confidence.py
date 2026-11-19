import math
from types import SimpleNamespace

import pytest
from recognition import Recognizer
from whisper_confidence import mean_log_probability, prefer_english
from test_recognition import FakeModel, speech


def result(text,language,p):
    return SimpleNamespace(text=text,language=language,tokens=[SimpleNamespace(id=10,p=p)])


@pytest.mark.parametrize('cs,en,expected',[(.6,.58,'en'),(.6,.5,'cs'),(.6,.6,'en')])
def test_confidence_and_english_bias_choose_decode(cs,en,expected):
    model=FakeModel('cs')
    def run(pcm,**options):
        model.calls.append(options)
        language=options['language'] or 'cs'
        return result(language,language,en if language=='en' else cs)
    model.run=run
    assert Recognizer(model,'en,cs').run(speech())==(expected,expected)
    assert [c['language'] for c in model.calls]==[None,'en']


def test_english_detection_is_one_run_and_singleton_is_respected():
    model=FakeModel('en')
    model.run=lambda pcm,**kw: (model.calls.append(kw) or result('yes','en',.8))
    assert Recognizer(model,'en,cs').run(speech())==('yes','en')
    assert len(model.calls)==1
    model=FakeModel('cs')
    assert Recognizer(model,'cs').run(speech())==('hello','cs')
    assert len(model.calls)==1


def test_score_excludes_special_tokens_and_missing_confidence_keeps_candidate():
    r=SimpleNamespace(tokens=[SimpleNamespace(id=10,p=.25),SimpleNamespace(id=11,p=1),SimpleNamespace(id=50257,p=1e-12)])
    assert mean_log_probability(object(),r)==pytest.approx(math.log(.25)/2)
    assert mean_log_probability(object(),SimpleNamespace(tokens=())) is None
    assert not prefer_english(None,-.2)
    assert not prefer_english(-.2,None)


def test_selection_runs_in_each_long_window():
    model=FakeModel('cs')
    model.run=lambda pcm,**kw: (model.calls.append(kw) or result('en' if kw['language']=='en' else 'cs',kw['language'] or 'cs',.7 if kw['language']=='en' else .6))
    text,language=Recognizer(model,'en,cs',lambda **kw:kw).run(speech()*61)
    assert text=='en en en' and language=='en'
    assert [c['language'] for c in model.calls]==[None,'en','en']*3


def test_native_trace_is_captured_before_the_english_run(monkeypatch):
    import ctypes
    import sys
    class Trace(ctypes.Structure):
        _fields_=[('t0_ms',ctypes.c_int64),('t1_ms',ctypes.c_int64),('avg_logprob',ctypes.c_float),('no_speech_triggered',ctypes.c_bool)]
    current=[-.1]
    def read(handle,index,ptr):
        ptr._obj.t1_ms=1000
        ptr._obj.avg_logprob=current[0]
        return 0
    lib=SimpleNamespace(transcribe_get_whisper_chunk_count=lambda h:1,
                        transcribe_whisper_chunk_trace_init=lambda ptr:None,
                        transcribe_get_whisper_chunk_trace=read)
    monkeypatch.setitem(sys.modules,'transcribe_cpp',SimpleNamespace(_lib=lib,_generated=SimpleNamespace(transcribe_whisper_chunk_trace=Trace)))
    model=FakeModel('cs');model._h=1
    def run(pcm,**options):
        current[0]=-.9 if options['language']=='en' else -.1
        return SimpleNamespace(text='en' if options['language']=='en' else 'cs',language=options['language'] or 'cs',tokens=())
    model.run=run
    assert Recognizer(model,'en,cs').run(speech())==('cs','cs')


@pytest.mark.parametrize('failure',['missing','status','exception'])
def test_trace_failure_returns_none_and_logs_once(monkeypatch,caplog,failure):
    import sys
    import whisper_confidence as wc
    monkeypatch.setattr(wc,'_warned',False)
    if failure=='missing':
        monkeypatch.setitem(sys.modules,'transcribe_cpp',SimpleNamespace(_lib=SimpleNamespace()))
    elif failure=='exception':
        monkeypatch.setitem(sys.modules,'transcribe_cpp',SimpleNamespace(_lib=SimpleNamespace(transcribe_get_whisper_chunk_count=lambda h: (_ for _ in ()).throw(OSError('probe failure')))))
    else:
        import ctypes
        class Trace(ctypes.Structure):_fields_=[('avg_logprob',ctypes.c_float)]
        lib=SimpleNamespace(transcribe_get_whisper_chunk_count=lambda h:1,transcribe_whisper_chunk_trace_init=lambda p:None,transcribe_get_whisper_chunk_trace=lambda *v:4)
        monkeypatch.setitem(sys.modules,'transcribe_cpp',SimpleNamespace(_lib=lib,_generated=SimpleNamespace(transcribe_whisper_chunk_trace=Trace)))
    session=SimpleNamespace(_h=1)
    for _ in range(2):assert wc.mean_log_probability(session,SimpleNamespace(tokens=())) is None
    assert sum('confidence unavailable' in r.message for r in caplog.records)==1


def test_failed_warmup_disables_confidence_and_keeps_detected_language(monkeypatch,caplog):
    import sys
    monkeypatch.setitem(sys.modules,'transcribe_cpp',SimpleNamespace(_lib=SimpleNamespace()))
    model=FakeModel('cs');model._h=1
    recognizer=Recognizer(model,'en,cs')
    with caplog.at_level('INFO'):
        recognizer.run(speech(),warmup=True)
    assert not recognizer.confidence_active
    assert recognizer.run(speech())==('hello','cs')
    assert len(model.calls)==2
    assert any('selection inactive' in r.message for r in caplog.records)


def test_fast_probe_winner_gets_a_normal_decode():
    model=FakeModel('cs')
    def run(pcm,**kw):
        model.calls.append(kw)
        if kw['language'] is None:return result('cs','cs',.6)
        fast=kw.get('family',{}).get('temperature_inc')==0
        return result('probe' if fast else 'proper English','en',.8)
    model.run=run
    assert Recognizer(model,'en,cs',lambda **v:v).run(speech())==('proper English','en')
    assert model.calls[1]['family']['temperature_inc']==0
    assert 'family' not in model.calls[2]


@pytest.mark.parametrize('failure_call',[2,3])
def test_optional_decode_failure_keeps_successful_candidate(caplog,failure_call):
    model=FakeModel('cs')
    def run(pcm,**kw):
        model.calls.append(kw)
        if len(model.calls)==failure_call:raise RuntimeError('optional failure')
        return result('Czech candidate' if kw['language'] is None else 'English probe',kw['language'] or 'cs',.6 if kw['language'] is None else .8)
    model.run=run
    assert Recognizer(model,'en,cs',lambda **v:v).run(speech())==('Czech candidate','cs')
    assert any('keeping candidate' in r.message for r in caplog.records)
    assert all('Czech candidate' not in r.message for r in caplog.records)


def test_trace_weights_consumed_audio_not_overlapping_encoder_windows(monkeypatch):
    import whisper_confidence as wc
    traces=[SimpleNamespace(t0_ms=0,t1_ms=30000,avg_logprob=-.1,no_speech_triggered=False),
            SimpleNamespace(t0_ms=1000,t1_ms=30000,avg_logprob=-2,no_speech_triggered=False)]
    monkeypatch.setattr(wc,'_native_traces',lambda session:traces)
    score=wc.mean_log_probability(object(),SimpleNamespace(tokens=()))
    assert score==pytest.approx((-0.1*1000-2*29000)/30000)
    assert wc.prefer_english(score,-1.5)
