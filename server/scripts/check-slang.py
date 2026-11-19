#!/usr/bin/env python3
"""Desktop-only Czech slang experiment; private voice transcripts stay in bench."""
import collections
import json
import os
from pathlib import Path
import re
import sys
import time
import unicodedata

SERVER=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(SERVER))
from gateway_state import read_glossary
from recognition import Recognizer,speech_evidence,no_speech_text
from voice_gateway import decode_audio,clean,PROMPT

VERBS='pushni mergni commitni deployni checkni rebasni pullni buildni restartni pingni fetchni squashni revertni debugni testni fixni releasni reviewni updatni cherry-pickni'.split()
LIST=', '.join(VERBS)+'.'
SENTENCE='Při vývoji často říkáme: '+', '.join(VERBS[:-1])+' a '+VERBS[-1]+'. Potom hodnotíme, co se podařilo pushnout nebo mergnout.'
ROOTS={'push':('push','pus'),'merge':('merge','merg','merz','merdz'),
       'commit':('commit','comit','komit','kommit'),'deploy':('deploy','deploj'),
       'check':('check','cek'),'rebase':('rebase','rebas','rebaz','rebejs','rebejz'),
       'pull':('pull','pul'),'build':('build','bild','bilt'),
       'restart':('restart',),'ping':('ping','pink')}
SUFFIXES=('ni','nout','nul','nula','nuli','neme','nete','nu','nes','ne','ovat')
BARE={'push':'push','puš':'push','merge':'merge','merg':'merge','merž':'merge','merdž':'merge',
      'commit':'commit','komit':'commit','deploy':'deploy','deploj':'deploy','check':'check','ček':'check',
      'rebase':'rebase','rebejs':'rebase','pull':'pull','build':'build','bild':'build','restart':'restart','ping':'ping'}


def family(word):
    # Ordinary Czech půl (half) and English pink are not bare dev verbs.
    if word.casefold() in BARE:return BARE[word.casefold()]
    word=''.join(c for c in unicodedata.normalize('NFD',word.casefold()) if not unicodedata.combining(c))
    for name,roots in ROOTS.items():
        if any(word==root+suffix for root in roots for suffix in SUFFIXES):return name
    return None


def verb_words(text):
    words=re.findall(r'[^\W_]+',text)
    for index,word in enumerate(words):
        # Pull/merge request are noun phrases, not extra slang verbs.
        if word.casefold() in ('pull','merge') and index+1<len(words) and words[index+1].casefold()=='request':continue
        if family(word):yield word.casefold()


def verbs(text):
    return collections.Counter(family(w) for w in verb_words(text))


def introduced_slang(text):
    # Check all 20 seed verbs, not just the ten families in the references.
    words=re.findall(r'[^\W_]+(?:-[^\W_]+)*',text.casefold())
    return sum(bool(family(w)) or any(w==v[:-2]+ending for v in VERBS for ending in SUFFIXES) for w in words)


def hits(reference,text):
    return sum((verbs(reference)&verbs(text)).values())


def prompted(recognizer,pcm,base,language,prompt,options_factory):
    # Admission, no-speech suppression and language selection have finished.
    # Prompted probabilities never participate in the frozen language choice.
    if language!='cs' or not base:return base
    try:
        result=recognizer.session.run(pcm,language='cs',timestamps='none',family=options_factory(initial_prompt=prompt,condition_on_prev_tokens=False))
        text=result.text.strip()
        if text and (speech_evidence(pcm)[1] or not no_speech_text(text)):return text
    except Exception as error:
        print('optional Czech prompt failed',type(error).__name__,flush=True)
    return base


def main():
    from bench.bench import normalize,score
    import av,numpy as np,transcribe_cpp as tc
    os.umask(0o077)
    bench=Path.home()/'.local/share/hark-bench';output=[]
    references=[line.split('\t',2) for line in (bench/'sentences.tsv').read_text().splitlines() if line.strip()]
    rows=[dict(key='clips/'+key,category=cat,ref=ref) for key,cat,ref in references if int(key)>=12]
    previous=[json.loads(line) for line in (bench/'results/language-round.jsonl').read_text().splitlines()]
    rows += [dict(key=r['key'],category='public-cs',ref=r['ref']) for r in previous if r['variant']=='none' and r['corpus']=='full' and r['key'].startswith('fleurs-cs/')]
    prompts={'a-list':LIST,'b-sentence':SENTENCE}
    glossary=read_glossary(Path.home()/'.local/share/voice-gateway/glossary.txt') if '--live-glossary' in sys.argv else read_glossary(SERVER/'glossary.txt')
    samples={r['key']:decode_audio(av,np,(bench/(r['key']+'.wav')).read_bytes()) for r in rows}
    with tc.Model(Path.home()/'.local/share/voice-gateway/models/whisper-large-v3-turbo-Q8_0.gguf') as model:
        rec=Recognizer(model,'en,cs',tc.WhisperRunOptions);rec.run(np.zeros(16000,dtype=np.float32),warmup=True)
        for r in rows:
            pcm=samples[r['key']]
            for repeat in range(3):
                began=time.perf_counter();base,language=rec.run(pcm);base_s=time.perf_counter()-began
                for name,prompt in [('today',None),*prompts.items()]:
                    began=time.perf_counter();text=base if prompt is None else prompted(rec,pcm,base,language,prompt,tc.WhisperRunOptions);extra_s=time.perf_counter()-began
                    output.append(dict(**r,repeat=repeat,variant=name,text=text,language=language,seconds=base_s+extra_s,scores=score(r['ref'],text),hits=hits(r['ref'],text),targets=sum(verbs(r['ref']).values()),introduced=introduced_slang(text) if r['key'] in ('clips/25','clips/26') else None))
            print('ASR',r['key'],'[private]',flush=True)
        # Choose solely between the two predeclared prompts; refs never enter
        # either prompt. Variant c uses that wording plus the selected glossary.
        def quality(name):
            slang=[r for r in output if r['variant']==name and r['category']=='cs+slang']
            return sum(r['hits'] for r in slang),-sum(r['scores']['word_errors'] for r in slang)
        best=max(prompts,key=quality)
        c_prompt=prompts[best]+' Názvy a termíny: '+', '.join(glossary)+'.'
        print('best prompt',best,flush=True)
        for r in rows:
            pcm=samples[r['key']]
            for repeat in range(3):
                began=time.perf_counter();base,language=rec.run(pcm);base_s=time.perf_counter()-began
                began=time.perf_counter();text=prompted(rec,pcm,base,language,c_prompt,tc.WhisperRunOptions);extra_s=time.perf_counter()-began
                output.append(dict(**r,repeat=repeat,variant='c-glossary',text=text,language=language,seconds=base_s+extra_s,scores=score(r['ref'],text),hits=hits(r['ref'],text),targets=sum(verbs(r['ref']).values()),introduced=introduced_slang(text) if r['key'] in ('clips/25','clips/26') else None))
            print('ASR glossary',r['key'],'[private]',flush=True)
        controls=Path.home()/'.cache/hark-build/czech/speech-check/audio'
        for name in ['silence2','silence10','noise','click']:
            pcm=decode_audio(av,np,(controls/(name+'.wav')).read_bytes());base,language=rec.run(pcm)
            for variant,prompt in {**prompts,'c-glossary':c_prompt}.items():
                text=prompted(rec,pcm,base,language,prompt,tc.WhisperRunOptions)
                output.append(dict(key=name,category='control',variant=variant,text=text,language=language))
    # Measure the current cleanup on every recognized slang variant, using the
    # same glossary without injecting an oracle list from clip references.
    for r in output:
        if r['category']!='cs+slang':continue
        text,status=clean(r['text'],glossary,prompt=PROMPT)
        before=verbs(r['text']);after=verbs(text)
        raw_words=collections.Counter(verb_words(r['text']))
        clean_words=collections.Counter(normalize(text).split())
        r.update(cleaned=text,cleanup=status,recognized=sum(before.values()),kept=sum((before&after).values()),forms_kept=sum((raw_words&clean_words).values()),cleanup_scores=score(r['ref'],text))
    path=bench/'results'/('slang-live-round.json' if '--live-glossary' in sys.argv else 'slang-round.json');path.write_text(json.dumps(dict(prompts={**prompts,'c-glossary':c_prompt},best=best,rows=output),ensure_ascii=False,indent=2))
    print('DONE slang experiment; private results',flush=True)


if __name__=='__main__':main()
