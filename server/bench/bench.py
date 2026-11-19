#!/usr/bin/env python3
"""Offline desktop bake-off. Audio stays on desktop; no gateway requests or history access.

Each ASR worker exits before the next model loads. Cleanup uses gateway.clean()
unchanged. JSONL checkpoints allow --cleanup-only and interrupted runs to resume.
"""
import argparse
import collections
import csv
import gc
import json
import os
import re
from pathlib import Path
import statistics
import subprocess
import sys
import threading
import time
import unicodedata

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from voice_gateway import PROMPT, clean, decode_audio

CZECH_PROMPT = PROMPT + '''
The dictation may be English, Czech, or mixed within a sentence. Keep each word
in the language spoken. Never translate or replace English technical words
with Czech equivalents, or Czech words with English equivalents. Correct Czech
diacritics, grammatical spelling, capitalization and punctuation conservatively.
Keep Czech proper names when surrounded by English. If a name is uncertain,
use a matching glossary spelling only when it fits the sound and context.
Recognize Czech spoken punctuation as well (tečka, čárka, otazník, nový řádek).
Do not paraphrase, invent facts, expand abbreviations, or change the word order.'''

# Whole-name occurrences, including the brand Kofola; common nouns are excluded.
NAMES = {
    '07': ['Jiří', 'Holešovice'],
    '08': ['Lokál', 'Karlín', 'Petr'],
    '09': ['Vinohrady', 'Smíchov', 'Anděl'],
    '10': ['Šárka', 'Kofola'],
    '11': ['České Budějovice', 'Přemysl', 'Ostrava'],
}
GLOSSARY_NAMES = {**NAMES, '14': ['Petrem'], '16': ['Brna']}
ROOT = Path.home()/'.local/share/hark-bench'
MODEL_PATHS = {
    'english': Path.home()/'.local/share/voice-gateway/models/parakeet-unified-en-0.6b-Q8_0.gguf',
    'v3': ROOT/'models/parakeet-tdt-0.6b-v3-Q8_0.gguf',
    'turbo': ROOT/'models/whisper-large-v3-turbo-Q8_0.gguf',
    'large': ROOT/'models/whisper-large-v3-Q8_0.gguf',
    'canary': ROOT/'models/canary-1b-v2-Q8_0.gguf',
    'qwen06': ROOT/'models/Qwen3-ASR-0.6B-Q8_0.gguf',
    'qwen17': ROOT/'models/Qwen3-ASR-1.7B-Q8_0.gguf',
    'nemotron': ROOT/'models/nemotron-3.5-asr-streaming-0.6b-Q8_0.gguf',
    'primeline': ROOT/'models/parakeet-primeline-Q8_0.gguf',
    'funasr': ROOT/'models/Fun-ASR-MLT-Nano-2512-Q8_0.gguf',
}

def language_for(model, category):
    if model == 'canary':
        return 'cs' if category.startswith('cs') else 'en'
    # 0.3.1's generic language validator rejects literal "auto" because it
    # is absent from general.languages. Null reaches Nemotron's auto slot.
    return None

def reported_language(result):
    if result.language:
        return result.language, 'result.language'
    # Nemotron auto mode reports locale as a decoded token, rather than through
    # Result.language in 0.3.1. Never infer a label from spelling or references.
    match = re.search(r'<(?:lang-)?([a-z]{2,3}-[A-Z]{2,4})>', result.raw_text)
    return (match.group(1), 'raw_token') if match else (None, None)

def run_checked(session, pcm, **options):
    try:
        return session.run(pcm, **options), 'ok'
    except Exception as error:
        # The binding explicitly preserves partial output for OutputTruncated,
        # OutputRepetition and Aborted. Score it and expose the failure rather
        # than silently dropping a hard clip or terminating the whole corpus.
        partial = getattr(error, 'partial_result', None)
        if partial is None:
            raise
        return partial, type(error).__name__

def normalize(value):
    value = unicodedata.normalize('NFC', value).casefold()
    # Remove punctuation rather than splitting apostrophes/hyphens into words.
    return ' '.join(''.join(c for c in value if not unicodedata.category(c).startswith('P')).split())

def distance(reference, hypothesis):
    previous = list(range(len(hypothesis)+1))
    for i, a in enumerate(reference, 1):
        current = [i]
        for j, b in enumerate(hypothesis, 1):
            current.append(min(current[-1]+1, previous[j]+1, previous[j-1]+(a != b)))
        previous = current
    return previous[-1]

def score(reference, text, names=()):
    ref, hyp = normalize(reference), normalize(text)
    words = ref.split()
    hits = sum((' '+normalize(name)+' ') in (' '+hyp+' ') for name in names)
    return dict(word_errors=distance(words, hyp.split()), words=len(words),
                char_errors=distance(ref, hyp), chars=len(ref),
                name_hits=hits, names=len(names))

def references(path):
    return [dict(id=n, category=c, reference=t) for n, c, t in csv.reader(path.open(), delimiter='\t') if n.strip()]

def gpu_memory():
    result = subprocess.run(['nvidia-smi', '--query-gpu=memory.used', '--format=csv,noheader,nounits', '--id=0'], capture_output=True, text=True, check=True)
    return int(result.stdout.strip())

def process_memory(pid):
    result = subprocess.run(['nvidia-smi', '--query-compute-apps=pid,used_memory', '--format=csv,noheader,nounits', '--id=0'], capture_output=True, text=True, check=True)
    for line in result.stdout.splitlines():
        fields = [field.strip() for field in line.split(',')]
        if fields[0] == str(pid) and fields[1].isdigit():
            return int(fields[1])
    return 0

class MemorySampler:
    """Whole-device memory, sampled at ~50ms plus nvidia-smi invocation time."""
    def __init__(self):
        self.baseline = gpu_memory()
        self.peak = self.baseline
        self.process_peak = process_memory(os.getpid())
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.poll, daemon=True)
    def poll(self):
        while not self.stop.is_set():
            self.peak = max(self.peak, gpu_memory())
            self.process_peak = max(self.process_peak, process_memory(os.getpid()))
            self.stop.wait(0.05)
    def __enter__(self):
        self.thread.start()
        return self
    def __exit__(self, *args):
        self.peak = max(self.peak, gpu_memory())
        self.process_peak = max(self.process_peak, process_memory(os.getpid()))
        self.stop.set()
        self.thread.join()

def write(path, row):
    with path.open('a') as out:
        out.write(json.dumps(row, ensure_ascii=False)+'\n')

def read(path):
    return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

def finish_worker(label):
    # Contexts have closed and JSONL writes have flushed. Process exit also
    # releases Vulkan device buffer pools and avoids NVIDIA global teardown.
    print('worker freed', label, flush=True)
    sys.stdout.flush()
    sys.stderr.flush()
    os._exit(0)

def worker(args):
    import av
    import numpy as np
    import transcribe_cpp as tc
    rows = references(args.references)
    existing = {r['id'] for r in read(args.output) if r.get('kind') == 'raw' and r['model'] == args.worker}
    todo = [r for r in rows if r['id'] not in existing]
    if not todo:
        return
    with MemorySampler() as memory:
        started = time.perf_counter()
        with tc.Model(MODEL_PATHS[args.worker], backend='vulkan') as model:
            with model.session(n_threads=4) as session:
                load_s = time.perf_counter()-started
                # Warm on real audio, avoiding silence hallucinations and measuring first-call cost.
                pcm = decode_audio(av, np, (args.clips/(todo[0]['id']+'.wav')).read_bytes())
                warm_language = language_for(args.worker, todo[0]['category'])
                started = time.perf_counter()
                run_checked(session, pcm, language=warm_language, timestamps='none')
                warmup_s = time.perf_counter()-started
                for row in todo:
                    pcm = decode_audio(av, np, (args.clips/(row['id']+'.wav')).read_bytes())
                    language = language_for(args.worker, row['category'])
                    times = []
                    outputs = []
                    statuses = []
                    for _ in range(args.repeats):
                        started = time.perf_counter()
                        result, status = run_checked(session, pcm, language=language, timestamps='none')
                        times.append(time.perf_counter()-started)
                        outputs.append(result.text.strip())
                        statuses.append(status)
                    detected, language_source = reported_language(result)
                    record = dict(row, kind='raw', model=args.worker, text=outputs[-1], model_raw_text=result.raw_text,
                                  detected_language=detected, language_source=language_source, forced_language=language,
                                  audio_s=len(pcm)/16000, asr_s=statistics.median(times),
                                  asr_repeats_s=times, repeat_text_equal=len(set(outputs)) == 1,
                                  asr_statuses=statuses,
                                  load_s=load_s, warmup_s=warmup_s,
                                  vram_baseline_mib=memory.baseline, vram_peak_mib=max(memory.peak, gpu_memory()),
                                  process_vram_peak_mib=max(memory.process_peak, process_memory(os.getpid())),
                                  engine_version=tc.native_version(), engine_commit=tc.native_commit(),
                                  scores=score(row['reference'], outputs[-1], NAMES.get(row['id'], ())))
                    write(args.output, record)
                    print(args.worker, row['id'], f"{record['asr_s']:.3f}s", flush=True)
    gc.collect()

def cleanup(args):
    glossary = [line.strip() for line in args.glossary.read_text().splitlines() if line.strip() and not line.strip().startswith('#')]
    # Store the exact inputs used for reproducibility; no history or credentials.
    inputs = dict(prompt_today=PROMPT, prompt_czech=CZECH_PROMPT, glossary=glossary, scored_names=NAMES, glossary_names=GLOSSARY_NAMES, repeats=args.repeats)
    input_path = args.output.with_suffix('.inputs.json')
    if input_path.exists() and json.loads(input_path.read_text()) != inputs:
        raise ValueError('Cleanup inputs changed; use a new output filename instead of mixing benchmark conditions')
    input_path.write_text(json.dumps(inputs, ensure_ascii=False, indent=2)+'\n')
    rows = read(args.output)
    done = {(r['model'], r['id'], r.get('variant')) for r in rows if r['kind'] == 'clean'}
    cleaned = {(r['model'], r['id'], r['variant']): r for r in rows if r['kind'] == 'clean'}
    raw = [r for r in rows if r['kind'] in ('raw', 'router')]
    # Warm the LLM without changing keep_alive or any Ollama configuration.
    clean('A short benchmark sentence.', glossary)
    for row in raw:
        for prompt_name, prompt in [('today', PROMPT), ('czech', CZECH_PROMPT)]:
            for augmented in (False, True):
                variant = prompt_name+('+names' if augmented else '')
                if (row['model'], row['id'], variant) in done:
                    continue
                terms = list(dict.fromkeys(glossary+(GLOSSARY_NAMES.get(row['id'], []) if augmented else [])))
                key = (row['model'], row['id'], variant)
                if augmented and terms == list(dict.fromkeys(glossary)):
                    # The prompt/request is literally identical when no new names
                    # exist. Reuse its measured result rather than fake a contrast.
                    record = dict(cleaned[(row['model'], row['id'], prompt_name)], variant=variant, identical_input_to=prompt_name)
                    write(args.output, record)
                    cleaned[key] = record
                    continue
                times = []
                for _ in range(args.cleanup_repeats):
                    started = time.perf_counter()
                    text, status = clean(row['text'], terms, prompt=prompt, drift_guard=True) if row['text'] else ('', 'skipped-empty')
                    times.append(time.perf_counter()-started)
                record = dict(row, kind='clean', variant=variant, raw_text=row['text'], text=text,
                                       cleanup_status=status, cleanup_s=statistics.median(times), cleanup_repeats_s=times,
                                       total_s=row['asr_s']+statistics.median(times),
                                       scores=score(row['reference'], text, NAMES.get(row['id'], ())))
                write(args.output, record)
                cleaned[key] = record
        print('cleanup', row['model'], row['id'], flush=True)

def route(args):
    """Join measured warm stages, then measure actual one-candidate-at-a-time swaps.

    Whisper must finish its full transcription to expose Result.language in this
    binding. This is a real router cost, not a standalone language-ID estimate.
    """
    import av
    import numpy as np
    import transcribe_cpp as tc
    rows = read(args.output)
    raw = {(r['model'], r['id']): r for r in rows if r['kind'] == 'raw'}
    done = {r['id'] for r in rows if r['kind'] == 'router'}
    for row in references(args.references):
        if row['id'] in done:
            continue
        detector = raw[('turbo', row['id'])]
        language = detector['detected_language']
        # Unknown/non-EN/CS languages conservatively select the multilingual target.
        target = 'english' if language == 'en' else 'canary'
        target_language = 'en' if language == 'en' else 'cs'
        pcm = decode_audio(av, np, (args.clips/(row['id']+'.wav')).read_bytes())
        started = time.perf_counter()
        with MemorySampler() as memory:
            with tc.Model(MODEL_PATHS['turbo'], backend='vulkan') as model:
                with model.session(n_threads=4) as session:
                    detected = session.run(pcm, timestamps='none').language
            # The detector is freed before the target is loaded.
            target = 'english' if detected == 'en' else 'canary'
            target_language = 'en' if detected == 'en' else 'cs'
            with tc.Model(MODEL_PATHS[target], backend='vulkan') as model:
                with model.session(n_threads=4) as session:
                    stage_started = time.perf_counter()
                    result = session.run(pcm, language=target_language if target == 'canary' else None, timestamps='none')
                    stage_s = time.perf_counter()-stage_started
            serial_s = time.perf_counter()-started
        matched = raw[(target, row['id'])]
        # Canary direct run used an oracle language; only borrow timing, never its
        # transcript, for a misrouted clip. Actual transcript above is scored.
        warm_s = detector['asr_s']+matched['asr_s'] if target != 'canary' or target_language == matched['forced_language'] else detector['asr_s']+stage_s
        write(args.output, dict(row, kind='router', model='router', text=result.text.strip(),
                               detected_language=detected or None, route_target=target,
                               forced_language=target_language if target == 'canary' else None,
                               asr_s=warm_s, serial_swap_s=serial_s, detector_s=detector['asr_s'],
                               target_stage_s=stage_s, audio_s=detector['audio_s'],
                               vram_baseline_mib=memory.baseline, vram_peak_mib=memory.peak,
                               process_vram_peak_mib=memory.process_peak,
                               scores=score(row['reference'], result.text, NAMES.get(row['id'], ()))))
        print('router', row['id'], detected, target, f'{warm_s:.3f}s warm, {serial_s:.3f}s swaps', flush=True)

def summary(path):
    groups = collections.defaultdict(list)
    for row in read(path):
        groups[(row['model'], row.get('variant', 'raw'), row['category'])].append(row)
    output = []
    for (model, variant, category), rows in sorted(groups.items()):
        counts = {key: sum(r['scores'][key] for r in rows) for key in ('word_errors', 'words', 'char_errors', 'chars', 'name_hits', 'names')}
        output.append(dict(model=model, variant=variant, category=category, clips=len(rows),
                           wer=100*counts['word_errors']/max(1, counts['words']),
                           cer=100*counts['char_errors']/max(1, counts['chars']),
                           name_accuracy=100*counts['name_hits']/counts['names'] if counts['names'] else None,
                           median_asr_s=statistics.median(r['asr_s'] for r in rows),
                           median_cleanup_s=statistics.median(r.get('cleanup_s', 0) for r in rows),
                           median_total_s=statistics.median(r.get('total_s', r['asr_s']) for r in rows),
                           median_serial_swap_s=statistics.median(r['serial_swap_s'] for r in rows) if model == 'router' else None,
                           peak_vram_mib=max(r['vram_peak_mib'] for r in rows),
                           cleanup_status=dict(collections.Counter(r.get('cleanup_status', 'raw') for r in rows)), **counts))
    path.with_suffix('.summary.json').write_text(json.dumps(output, ensure_ascii=False, indent=2)+'\n')

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--clips', type=Path, required=True)
    parser.add_argument('--references', type=Path, default=Path(__file__).with_name('sentences.tsv'))
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--glossary', type=Path, default=Path.home()/'.local/share/voice-gateway/glossary.txt')
    parser.add_argument('--models', nargs='+', choices=list(MODEL_PATHS), default=list(MODEL_PATHS))
    parser.add_argument('--repeats', type=int, default=3)
    parser.add_argument('--cleanup-repeats', type=int, default=2)
    parser.add_argument('--worker', choices=list(MODEL_PATHS))
    parser.add_argument('--router-worker', action='store_true')
    parser.add_argument('--cleanup-only', action='store_true')
    parser.add_argument('--skip-cleanup', action='store_true')
    parser.add_argument('--skip-router', action='store_true')
    args = parser.parse_args()
    os.umask(0o077)
    # A partial user corpus must never silently become a scored full corpus.
    missing = [r['id'] for r in references(args.references) if not (args.clips/(r['id']+'.wav')).is_file()]
    if missing:
        parser.error('Missing clips: '+', '.join(missing))
    if args.repeats < 1 or args.cleanup_repeats < 1:
        parser.error('Repeat counts must be positive')
    args.output.parent.mkdir(parents=True, exist_ok=True)
    if args.worker:
        worker(args)
        # All contexts and the sampler have closed and every JSONL write is
        # flushed. NVIDIA 615 can spin in process-global Vulkan/GLX teardown
        # after Qwen 1.7B; let the OS reclaim the worker's remaining driver
        # objects without running unrelated library-global destructors.
        finish_worker(args.worker)
    if args.router_worker:
        route(args)
        finish_worker('router')
    if not args.cleanup_only:
        for model in args.models:
            clean('A short benchmark sentence.', [])  # Keep qwen3:8b resident for ASR VRAM measurements.
            subprocess.run([sys.executable, __file__, '--worker', model, '--clips', str(args.clips), '--references', str(args.references), '--output', str(args.output), '--repeats', str(args.repeats)], check=True)
        if not args.skip_router:
            subprocess.run([sys.executable, __file__, '--router-worker', '--clips', str(args.clips), '--references', str(args.references), '--output', str(args.output)], check=True)
    if not args.skip_cleanup:
        cleanup(args)
    summary(args.output)

if __name__ == '__main__':
    main()
