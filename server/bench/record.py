#!/usr/bin/env python3
"""Record the bake-off clips: read each sentence aloud, one WAV per sentence.

Clips go to ~/.local/share/hark-bench/clips (mode 700) and are deleted once a
model is chosen. Uses the default microphone, the same one hark uses.
"""

import os
import signal
import subprocess
import sys
from pathlib import Path

CLIPS = Path.home() / ".local/share/hark-bench/clips"
SENTENCES = Path(__file__).with_name("sentences.tsv")


def record(path):
    input("  Press Enter, then read the sentence...")
    process = subprocess.Popen(["pw-record", "--rate", "16000", "--channels", "1", "--format", "s16", str(path)])
    input("  Recording. Press Enter when you finish.")
    process.send_signal(signal.SIGINT)  # pw-record finalises the WAV header on SIGINT
    process.wait()


def main():
    CLIPS.mkdir(parents=True, exist_ok=True)
    os.chmod(CLIPS.parent, 0o700)
    os.chmod(CLIPS, 0o700)
    rows = [line.split("\t") for line in SENTENCES.read_text(encoding="utf-8").splitlines() if line.strip()]
    only = set(sys.argv[1:])
    print("Speak naturally, as you would dictate. Type r + Enter after a clip to redo it.\n")
    for number, kind, text in rows:
        if only and number not in only:
            continue
        path = CLIPS / f"{number}.wav"
        while True:
            print(f"[{number}/{len(rows)}] ({kind})\n  {text}")
            record(path)
            if input("  Enter = next, r = redo: ").strip().lower() != "r":
                break
        print()
    print(f"Done: {len(list(CLIPS.glob('*.wav')))} clips in {CLIPS}")


if __name__ == "__main__":
    main()
