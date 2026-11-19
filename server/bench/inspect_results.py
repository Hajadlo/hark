#!/usr/bin/env python3
"""Export aggregate diagnostics only; never exports audio or whole transcripts."""
import collections
import json
import math
from pathlib import Path
import statistics
import urllib.request

def diagnostics(path):
    rows = [json.loads(line) for line in path.read_text().splitlines()]
    raw = [r for r in rows if r['kind'] in ('raw', 'router')]
    groups = collections.defaultdict(list)
    for row in raw:
        groups[row['model']].append(row)
    result = {}
    for model, values in groups.items():
        durations = sorted(r['asr_s'] for r in values)
        result[model] = dict(
            clips=len(values), median_asr_s=statistics.median(durations),
            p95_asr_s=durations[math.ceil(len(durations)*0.95)-1],
            peak_vram_mib=max(r['vram_peak_mib'] for r in values),
            baseline_min_mib=min(r['vram_baseline_mib'] for r in values),
            baseline_max_mib=max(r['vram_baseline_mib'] for r in values),
            incremental_max_mib=max(r['vram_peak_mib']-r['vram_baseline_mib'] for r in values),
            process_peak_mib=max((r['process_vram_peak_mib'] for r in values if 'process_vram_peak_mib' in r), default=None),
            languages=dict(collections.Counter(r.get('detected_language') for r in values)),
            repeat_text_changes=sum(not r.get('repeat_text_equal', True) for r in values),
            non_ok_clips=sum(any(s != 'ok' for s in r.get('asr_statuses', [])) for r in values),
            empty_clips=sum(not r['text'] for r in values),
        )
        if model == 'router':
            result[model]['median_serial_swap_s'] = statistics.median(r['serial_swap_s'] for r in values)
            result[model]['routes'] = dict(collections.Counter(r['route_target'] for r in values))
        else:
            result[model]['load_s'] = values[0]['load_s']
            result[model]['warmup_s'] = values[0]['warmup_s']
        for variant in ('today', 'today+names', 'czech', 'czech+names'):
            cleaned = [r for r in rows if r['kind'] == 'clean' and r['model'] == model and r['variant'] == variant]
            if cleaned:
                result[model][variant] = dict(
                    median_cleanup_s=statistics.median(r['cleanup_s'] for r in cleaned),
                    median_total_s=statistics.median(r['total_s'] for r in cleaned),
                    status=dict(collections.Counter(r['cleanup_status'] for r in cleaned)),
                    identical_inputs=sum('identical_input_to' in r for r in cleaned),
                )
    references = {r['id']: r['audio_s'] for r in raw}
    return dict(models=result, audio_total_s=sum(references.values()),
                clip_durations_s=references, kinds=dict(collections.Counter(r['kind'] for r in rows)))

def main():
    root = Path.home()/'.local/share/hark-bench'
    for corpus in ('synthetic', 'fleurs-cs', 'clips'):
        path = root/'results'/(corpus+'.jsonl')
        if path.exists():
            path.with_suffix('.diagnostics.json').write_text(json.dumps(diagnostics(path), ensure_ascii=False, indent=2)+'\n')
    with urllib.request.urlopen('http://127.0.0.1:11434/api/ps') as response:
        active = json.load(response)
    (root/'results/ollama-residency.json').write_text(json.dumps(active, indent=2)+'\n')

if __name__ == '__main__':
    main()
