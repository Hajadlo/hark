# Czech bake-off

Run it on the GPU host. `bench.py` uses the existing gateway venv
read-only (transcribe-cpp 0.3.1, av, numpy); it installs nothing. Its only
network inference calls go to local Ollama, with the gateway's unchanged
`clean()` function and drift guard. No gateway transcription API calls, history access,
token reads, microphone changes, clipboard changes or phone interaction.

Prepare the models and corpora, then score a folder of clips against a TSV
of reference sentences:

```sh
python server/bench/prepare.py
python server/bench/bench.py --clips <dir> --references server/bench/sentences.tsv --output results.jsonl
```

Do not launch two benches concurrently.

Preparation pins the Handy GGUF repository revisions, verifies size and LFS
SHA256, and writes a model manifest. It downloads Q8 builds for the chosen
multilingual candidates, creates 16 espeak-ng sentences (`en` voice for `en`
and `en+cs`, `cs` for `cs+en`, speed 155), resamples them to mono 16 kHz PCM,
and extracts the first 20 rows of the published FLEURS Czech test TSV from
its audio archive. There is no cherry-picking or inference-based selection.
FLEURS has its own `cs` category: those public clips are ordinary Czech,
not code-switching examples. Synthetic audio is only a pipeline check.

For each model the harness warms on the first real clip, then takes the
median of three transcriptions per clip. Canary gets the reference's
dominant language (`en` or `cs`); its standalone scores are an oracle
language upper bound. Nemotron uses the implicit auto slot (no hint); literal `auto` is
rejected by 0.3.1's generic language validator. Other multilingual models
also get no hint. ASR defaults are otherwise unchanged, with timestamps
off and four CPU threads. Each candidate runs in a separate subprocess
and closes both session and model before the next candidate. The router has
its own worker too, so Vulkan device pools do not persist during cleanup.
Workers flush their output and exit via `os._exit(0)` after closing contexts;
this avoids a measured NVIDIA 615 global-finalizer stall.

Every raw transcript gets two prompts × two glossary settings: today's
prompt and a Czech-aware addition, with today's glossary and with that
clip's known Czech proper names appended. This is **reference-assisted
glossary augmentation**, an optimistic test of the Corrections → glossary
loop, not an ASR hotword test or a blind deployment score. Two cleanup calls
per unique setting measure the median latency; the last transcript is scored.
The exact prompts and glossary are saved alongside the results, and a
resume with changed cleanup inputs fails instead of mixing conditions.
When adding names adds no new terms, that
variant reuses the base variant's measurement and records `identical_input_to`.
Inflected Czech names in clips 14 and 16 also enter the augmented glossary;
proper-name scoring remains limited to clips 07–11. Qwen3:8b
is warmed before each candidate without overriding its normal keep-alive.

The router uses Whisper turbo's reported language; `en` selects the current
English model, every other/unknown label selects Canary with `cs`. It must
finish Whisper's full transcription because this binding does not expose
a detection-only call. Both the sum of warm stage measurements and an
actual detector-load → infer → free → target-load → infer → free duration
are recorded. Wrong routes are scored from the actual selected target
output. Canary's oracle transcript is never substituted for a routed one.

WER and CER use NFC, casefolding, punctuation removal and collapsed
whitespace, **retaining diacritics and literal numbers**. CER includes
normalized spaces. Aggregate scores sum edit counts and reference lengths;
they do not average per-clip percentages. Name accuracy requires a complete
normalized token sequence: 13 names/brands in clips 07–11, excluding the
common noun `chlebíčky`. It ignores capitalization/punctuation but retains
diacritics. Cleanup's digit conversion can therefore count as an error
against a spelled-number reference even when desirable for dictation.

VRAM is whole-device `nvidia-smi` memory sampled at about 50 ms plus command
overhead, covering model load, warmup and inference. The baseline and peak
are saved. The personal-corpus pass also samples the ASR worker's PID
memory, separating it from desktop activity. The desktop's live gateway
and GUI remain resident; whole-device peak minus baseline is only an
approximation of the candidate's incremental use. This is not an exact
allocator high-water mark and is not an RX 6600 measurement.

JSONL checkpoints and summaries stay under the desktop's hark-bench state.
Only text results and summaries may be copied back; **never sync `clips/`
or copy the user's WAV files**. Existing raw/clean rows are skipped on
resume. To change prompts, models, references or metric rules, use a new
output filename instead of mixing old and new rows. `--cleanup-only`
can rerun missing cleanup rows without reloading ASR. A corpus with missing
files fails before inference.

Metric tests: `uv run -q --with pytest pytest -q
server/bench/test_bench.py`. No model dependencies are imported by these
tests.

Native repetition/truncation exceptions preserve partial transcripts, which
are scored and marked in `asr_statuses`; they are not treated as successful
recognition. Empty output skips cleanup. `inspect_results.py` creates
aggregate latency, VRAM, language, empty-output and failure diagnostics
without exporting whole transcripts.

If the desktop does not answer, stop and report it. Never wake, reboot or
shut down either client machine; those are the user's decisions.
