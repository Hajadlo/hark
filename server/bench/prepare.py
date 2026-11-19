#!/usr/bin/env python3
"""Desktop-only model downloads and reproducible smoke corpus; never touches clips/."""
import argparse
import csv
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import urllib.request
import wave

MODELS = {
    'parakeet-tdt-0.6b-v3': '90f082450fcbacdb54e5900c44ef697c9ea59622',
    'whisper-large-v3-turbo': 'ceea6c8a94a21ab85be244d311e874a39344dbf5',
    'whisper-large-v3': 'b33a05f1459f33b0c876f014a57d51618b77d754',
    'canary-1b-v2': 'e2d8e6d7f2accc1259dc5497b517b4083047e44b',
    'Qwen3-ASR-0.6B': '28c66fba077f1359a42b52c55b618a24cdb6e0e7',
    'Qwen3-ASR-1.7B': '3555bd238a8572bbace3ebf60d23b036dc0a5dbe',
    'nemotron-3.5-asr-streaming-0.6b': '8139c4ec14bdc45c361adf8d57c27c28e7478272',
    'parakeet-primeline': '7c63545b352e5d410ccc1ea1eb49aa751e65780b',
    'Fun-ASR-MLT-Nano-2512': '364640227abd3f786d139ac43b8ac1a98673989d',
}

def fetch(url, path):
    temporary = path.with_suffix(path.suffix + '.partial')
    with urllib.request.urlopen(url, timeout=120) as source, temporary.open('wb') as target:
        shutil.copyfileobj(source, target)
    temporary.replace(path)

def wav16(source, target):
    import av
    import numpy as np
    parts = []
    with av.open(source) as container:
        resampler = av.AudioResampler(format='s16', layout='mono', rate=16000)
        for frame in container.decode(audio=0):
            parts.extend(f.to_ndarray().ravel() for f in resampler.resample(frame))
        parts.extend(f.to_ndarray().ravel() for f in resampler.resample(None))
    with wave.open(str(target), 'wb') as out:
        out.setparams((1, 2, 16000, 0, 'NONE', 'not compressed'))
        out.writeframes(np.concatenate(parts).astype('<i2').tobytes())

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=Path.home()/'.local/share/hark-bench')
    parser.add_argument('--skip-models', action='store_true')
    args = parser.parse_args()
    os.umask(0o077)
    root = args.root
    for name in ('models', 'synthetic', 'fleurs-cs', 'results'):
        (root/name).mkdir(parents=True, exist_ok=True)
        (root/name).chmod(0o700)
    root.chmod(0o700)
    if not args.skip_models:
        manifest = []
        for slug, revision in MODELS.items():
            filename = slug+'-Q8_0.gguf'
            repo = 'handy-computer/'+slug+'-gguf'
            metadata = json.load(urllib.request.urlopen('https://huggingface.co/api/models/'+repo+'/revision/'+revision+'?blobs=true'))
            info = next(s for s in metadata['siblings'] if s['rfilename'] == filename)
            path = root/'models'/filename
            url = 'https://huggingface.co/'+repo+'/resolve/'+revision+'/'+filename
            if not path.exists() or path.stat().st_size != info['size']:
                print('Downloading', filename, info['size'], flush=True)
                fetch(url, path)
            digest = hashlib.file_digest(path.open('rb'), 'sha256').hexdigest()
            expected = info.get('lfs', {}).get('sha256')
            if expected and digest != expected:
                raise ValueError('SHA256 mismatch: '+filename)
            manifest.append(dict(file=filename, bytes=path.stat().st_size, sha256=digest, revision=revision, url=url))
            print('Verified', filename, flush=True)
        (root/'models/manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
    references = Path(__file__).with_name('sentences.tsv')
    for number, category, text in csv.reader(references.open(), delimiter='\t'):
        intermediate = root/'synthetic'/f'{number}.source.wav'
        subprocess.run(['espeak-ng', '-v', 'cs' if category == 'cs+en' else 'en', '-s', '155', '-w', str(intermediate), text], check=True)
        wav16(intermediate, root/'synthetic'/f'{number}.wav')
        intermediate.unlink()
    shutil.copyfile(references, root/'synthetic/sentences.tsv')
    revision = '70bb2e84b976b7e960aa89f1c648e09c59f894dd'
    url = 'https://huggingface.co/datasets/google/fleurs/resolve/'+revision+'/data/cs_cz/'
    with urllib.request.urlopen(url+'test.tsv', timeout=120) as response:
        selected = list(csv.reader(response.read().decode().splitlines(), delimiter='\t'))[:20]
    wanted = {r[1] for r in selected}
    archive = root/'fleurs-cs/test.tar.gz'
    if not archive.exists():
        print('Downloading published FLEURS Czech test archive (451 MB)', flush=True)
        fetch(url+'audio/test.tar.gz', archive)
    # Extract only the 20 selected utterances, with no archive-controlled paths.
    with tarfile.open(archive) as tar:
        for member in tar:
            name = Path(member.name).name
            if member.isfile() and name in wanted:
                with tar.extractfile(member) as source, (root/'fleurs-cs'/name).open('wb') as target:
                    shutil.copyfileobj(source, target)
                wanted.remove(name)
    if wanted:
        raise ValueError('Missing FLEURS audio: '+str(wanted))
    rows = []
    provenance = []
    for index, row in enumerate(selected):
        number = f'f{index:03}'
        source = root/'fleurs-cs'/row[1]
        wav16(source, root/'fleurs-cs'/f'{number}.wav')
        source.unlink()
        rows.append((number, 'cs', row[2]))
        provenance.append(dict(id=number, row_idx=index, fleurs_id=row[0], source_file=row[1], reference=row[2]))
    with (root/'fleurs-cs/sentences.tsv').open('w') as out:
        csv.writer(out, delimiter='\t').writerows(rows)
    (root/'fleurs-cs/provenance.json').write_text(json.dumps(dict(source=url, rows=provenance), ensure_ascii=False, indent=2)+'\n')
    print('Prepared 16 synthetic and', len(rows), 'public Czech clips', flush=True)

if __name__ == '__main__':
    main()
