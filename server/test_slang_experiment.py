"""Keep the research prompt separate from production language selection."""
import importlib.util
from pathlib import Path
from types import SimpleNamespace

from recognition import Recognizer,czech_initial_prompt,CZECH_SLANG_PROMPT,CZECH_NOUN_PROMPT
from test_recognition import FakeModel,speech

spec=importlib.util.spec_from_file_location('slang_check',Path(__file__).with_name('scripts')/'check-slang.py')
slang=importlib.util.module_from_spec(spec);spec.loader.exec_module(slang)


def model_for(label,english_probability):
    model=FakeModel(label)
    def run(pcm,**kw):
        model.calls.append(kw)
        language=kw['language'] or label
        hinted=bool(kw.get('family',{}).get('initial_prompt'))
        # A hint can change its score/label but must not reopen selection.
        return SimpleNamespace(text='hinted' if hinted else language,language='en' if hinted else language,
                               tokens=[SimpleNamespace(id=10,p=.001 if hinted else english_probability if language=='en' else .8)])
    model.run=run
    return model


def test_prompt_only_after_czech_choice_without_changing_selection():
    model=model_for('cs',.5)
    rec=Recognizer(model,'en,cs',lambda **kw:kw)
    assert rec.run(speech(),czech_prompt=slang.LIST)==('hinted','cs')
    assert [r['language'] for r in model.calls]==[None,'en','cs']
    assert all(not r.get('family',{}).get('initial_prompt') for r in model.calls[:2])
    assert model.calls[2]['family']['initial_prompt']==slang.LIST


def test_english_initial_label_never_gets_the_prompt():
    model=model_for('en',.8);rec=Recognizer(model,'en,cs',lambda **kw:kw)
    assert rec.run(speech(),czech_prompt=slang.LIST)==('en','en')
    assert len(model.calls)==1
    assert not model.calls[0].get('family',{}).get('initial_prompt')


def test_english_winner_never_gets_the_prompt():
    model=model_for('cs',.95);rec=Recognizer(model,'en,cs',lambda **kw:kw)
    assert rec.run(speech(),czech_prompt=slang.LIST)==('en','en')
    assert [r['language'] for r in model.calls]==[None,'en','en']
    assert all(not r.get('family',{}).get('initial_prompt') for r in model.calls)


def test_empty_output_is_never_prompted():
    model=model_for('cs',.5)
    original=model.run
    def run(pcm,**kw):
        result=original(pcm,**kw)
        if kw['language'] is None:result.text=''
        return result
    model.run=run
    assert Recognizer(model,'en,cs',lambda **kw:kw).run(speech(),czech_prompt=slang.LIST)==('',None)
    assert len(model.calls)==2


def test_czech_hint_failure_retains_candidate(caplog):
    model=model_for('cs',.5)
    original=model.run
    def run(pcm,**kw):
        if kw.get('family',{}).get('initial_prompt'):raise RuntimeError('private text must not be logged')
        return original(pcm,**kw)
    model.run=run
    assert Recognizer(model,'en,cs',lambda **kw:kw).run(speech(),czech_prompt=slang.LIST)==('cs','cs')
    assert any('Optional Czech prompt failed' in r.message for r in caplog.records)
    assert all('private text' not in r.message for r in caplog.records)


def test_prompt_applies_per_czech_window():
    model=model_for('cs',.5)
    text,language=Recognizer(model,'en,cs',lambda **kw:kw).run(speech()*61,czech_prompt=slang.LIST)
    assert text=='hinted hinted hinted' and language=='cs'
    assert [r['language'] for r in model.calls]==[None,'en','cs']*3
    hints=[r for r in model.calls if r.get('family',{}).get('initial_prompt')]
    assert all(r['timestamps']=='segment' and not r['family']['condition_on_prev_tokens'] for r in hints)


def test_glossary_is_in_the_measured_prompt_and_empty_disables_it():
    assert CZECH_SLANG_PROMPT==slang.LIST
    assert czech_initial_prompt(['Example','Jiří'])==slang.LIST+' '+CZECH_NOUN_PROMPT+' Názvy a termíny: Example, Jiří.'
    assert czech_initial_prompt([]) is None


def test_hit_count_excludes_pull_request_noun_phrase():
    assert slang.verbs('Pushni ten branch a otevři pull request.')=={'push':1}
    assert slang.verbs('Pullni si nejnovější verzi a buildni to znovu.')=={'pull':1,'build':1}
    assert not slang.verbs('Pošli mi ten soubor poštou. Pustil jsem to znovu.')
    assert not slang.verbs('Půl je pink.')
    assert slang.verbs('puš pushni pušni pushnout')=={'push':4}
    assert slang.introduced_slang('fetchni debugnout cherry-pickni')==3
    assert slang.introduced_slang('Pošli poštou. Pustil jsem to.')==0


def test_gateway_reads_glossary_again_for_next_dictation(monkeypatch,tmp_path):
    import sys
    import voice_gateway as vg
    model=model_for('cs',.5)
    monkeypatch.setitem(sys.modules,'av',SimpleNamespace(error=SimpleNamespace(FFmpegError=ValueError)))
    monkeypatch.setitem(sys.modules,'transcribe_cpp',SimpleNamespace(Model=lambda path:model,WhisperRunOptions=lambda **kw:kw))
    monkeypatch.setattr(vg,'STATE',tmp_path)
    monkeypatch.setattr(vg,'decode_audio',lambda av,np,data:speech())
    glossary=tmp_path/'glossary.txt';glossary.write_text('FirstTerm\n')
    transcribe=vg.load_transcriber()
    assert transcribe(b'audio')==('hinted',1.0,'cs')
    assert 'FirstTerm' in model.calls[-1]['family']['initial_prompt']
    glossary.write_text('SecondTerm\n')
    assert transcribe(b'audio')==('hinted',1.0,'cs')
    assert 'SecondTerm' in model.calls[-1]['family']['initial_prompt']
    assert 'FirstTerm' not in model.calls[-1]['family']['initial_prompt']


def test_hint_preserves_opening_name_and_later_sentence_capitals():
    from recognition import preserve_capitals
    assert preserve_capitals('Poště to pro Jiřího. Mergni to.', 'pushni to pro jiřího. mergni to.')=='Pushni to pro Jiřího. Mergni to.'
    assert preserve_capitals('Petr používá GitHub a CI.', 'petr používá github a ci.')=='Petr používá GitHub a CI.'
    assert preserve_capitals('Ahoj. Poště to.', 'ahoj. pushni to.')=='Ahoj. Pushni to.'
    assert preserve_capitals('Dnes čeká Holoshowice.', 'dnes čeká holešovice.')=='Dnes čeká Holešovice.'
    assert preserve_capitals('Ahoj. Merge ni to.', 'ahoj. mergni to.')=='Ahoj. Mergni to.'
    assert preserve_capitals('Pushni to.', 'PUSHNI to.')=='PUSHNI to.'
    assert preserve_capitals('', 'hinted')=='hinted'


def test_warmup_runs_forced_and_hinted_czech_once(caplog):
    model=model_for('en',.8)
    rec=Recognizer(model,'en,cs',lambda **kw:kw)
    rec.warmup(speech(),czech_prompt='Example')
    assert [r['language'] for r in model.calls]==[None,'cs','cs']
    assert not model.calls[1].get('family',{}).get('initial_prompt')
    assert model.calls[2]['family']==dict(initial_prompt='Example',condition_on_prev_tokens=False)


def test_english_only_warmup_keeps_one_run():
    model=model_for('en',.8)
    Recognizer(model,'en',lambda **kw:kw).warmup(speech(),czech_prompt='Example')
    assert len(model.calls)==1 and model.calls[0]['language']=='en'


def test_prompt_budget_keeps_seeds_and_recent_complete_glossary_entries():
    tokenize=lambda text:text.split()
    terms=['Term'+str(i) for i in range(500)]
    prompt=czech_initial_prompt(terms,tokenize)
    assert len(tokenize(' '+prompt))<=223
    assert CZECH_SLANG_PROMPT in prompt and CZECH_NOUN_PROMPT in prompt
    assert 'Term499' in prompt and 'Term0,' not in prompt
    assert czech_initial_prompt(['x '*300],tokenize) is None


def test_warmup_hint_failure_is_optional(caplog):
    model=model_for('en',.8);original=model.run
    def run(pcm,**kw):
        if kw.get('family',{}).get('initial_prompt'):raise RuntimeError('private')
        return original(pcm,**kw)
    model.run=run
    Recognizer(model,'en,cs',lambda **kw:kw).warmup(speech(),czech_prompt='Example')
    assert any('Optional Czech warm-up failed' in r.message for r in caplog.records)
    assert all('private' not in r.message for r in caplog.records)
