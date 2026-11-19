# Voice gateway (hark server)

A private dictation endpoint on the GPU machine. The phone and the laptop
upload a recording. The gateway does three things:

- transcribes it with the configured GGUF (Parakeet English by default,
  or Whisper large-v3-turbo for English and Czech);
- cleans the text with a local LLM;
- stores both versions in searchable SQLite history.

It returns the text to the client, which types it into the focused field (for
example the T3 composer).

## How it works

- **API:** OpenAI's `POST /v1/audio/transcriptions` (multipart `file`,
  `model`, optional `response_format=text`), bearer token.
  - `model` selects a mode from the state's `modes.toml`, re-read on every
    request. `raw` has no prompt; `clean` keeps English/Czech words in their
    spoken language and fixes spelling, punctuation and filler words.
    Unknown names (including `whisper-1`) resolve to `clean`.
  - User modes may supply `description`, `prompt` and `drift_guard=false`.
    Modes without a prompt skip cleanup. Missing built-ins are filled in;
    a broken TOML file keeps the last valid version.
  - JSON replies include `text`, the history `id` (null when nothing was
    heard), the resolved `mode`, the `cleanup` status, and nullable `language`.
  - `GET /health` needs no token. `GET /v1/models` lists modes in file order,
    followed by any omitted built-ins.
- **Speech to text:** [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp)
  (`transcribe-cpp==0.3.1`, ggml on Vulkan). `VOICE_GATEWAY_ASR_MODEL` chooses
  the GGUF; its default remains `parakeet-unified-en-0.6b-Q8_0.gguf`.
  The same build runs on NVIDIA and AMD. The glossary goes to cleanup and
  to the final Czech decode.
  Whisper detects English/Czech automatically, with no language key or mode.
  `VOICE_GATEWAY_LANGUAGES` (default `en,cs`) restricts its decoder language:
  an outside detection triggers another decode forced to `cs` for Slavic
  labels (`sk,pl,sl,hr,sr,bs,ru,uk,be,bg,mk`), otherwise `en`. If that choice
  is absent from the configured set, the first allowed code is used. A single
  allowed code is forced immediately. Invalid/unsupported codes fail startup.
  English-only models ignore the setting and report `language: null`.
  The binding exposes only the winning label, so restriction can cost a
  second ASR run. When the closest allowed Whisper label is not `en` and
  English is allowed, the gateway also decodes forced English. English wins
  when its mean log probability plus **0.05** reaches the other decode's score.
  An initial `en` label keeps its single decode, and a singleton language
  setting is honored. The installed 0.3.1 Whisper path returns no token rows;
  a small adapter reads its generated C chunk trace's accepted mean log p,
  weighted by consumed audio up to the next trace start rather than overlapping
  encoder windows.
  This is pinned-binding code, covered by native desktop checks. Warm-up logs
  whether confidence selection is active. If its trace API is missing or fails,
  the gateway logs one warning and keeps the detected/closest allowed language.
  English latency remains unchanged. The extra English probe uses one greedy
  pass (`temperature_inc=0`) to avoid expensive wrong-language temperature
  fallback. If English wins, it is decoded again with normal options before
  returning text; Czech requests that stay Czech pay only for the cheap probe.
  If either optional English run raises, the successful candidate transcript
  and language are kept and a warning logs only the exception type.
  The same comparison applies per long window. No new env setting is needed.
  Prompts before language selection remain disabled: the measured glossary
  did not improve automatic name spelling and destabilized confidence choices.
  After a final **Czech** choice, Whisper decodes once more forced to `cs`,
  using 20 general dev slang verbs, 13 inflected technical nouns, and the same
  glossary cleanup reads. The hint helps spelling but does not guarantee it;
  the recorded `do mainu` case still has errors.
  The glossary is read per request, so additions apply to the next dictation.
  Prompt-conditioned scores never reopen selection; English detections and
  English winners receive no prompt or extra run. This applies per long window.
  Empty/no-speech output skips the hint; a failed or empty optional decode
  retains the candidate. If the glossary is absent/empty, the hint is disabled
  because the verb list alone regressed public Czech. English-only models
  ignore the hint. No new environment setting is needed.
  The hinted output retains capitals from the unhinted candidate: its opening
  word, aligned names (including mixed case), and later sentence starts.
  Only case changes; hinted spelling and punctuation remain intact.
  The native tokenizer counts the mandatory leading space and limits prompt
  text to 223 tokens (plus the previous-context marker). The current 31-term
  glossary fits entirely at 216 tokens. On overflow, oldest whole glossary
  entries are omitted from ASR, keeping the dev seeds and newest entries;
  cleanup still receives the full glossary. If no complete entry fits, the
  optional hint is disabled.
  Startup warms detection, forced Czech and the hinted Czech decoder once.
  Optional Czech warm-up failures log their exception type and do not prevent
  startup. English-only models keep their original warm-up. Three isolated
  restart checks added about 0.5 s to readiness (0.5 s polling resolution);
  first Czech ASR was 0.243 s before / 0.242 s after. The reported 2.02 s
  first request could not be reproduced without clearing shared GPU caches,
  which the test did not do. Warm Czech HTTP p50/p95 held at 0.248/0.274 s
  before and 0.249/0.277 s after.
  On the RTX 5080, the selected hint raised Czech-path warm ASR p50/p95 from
  0.189/0.239 s to 0.280/0.361 s, improving measured slang hits from 8/13 to
  11/13 without worsening public Czech raw WER.
  Whisper recordings over 30 s use separate windows with fresh language
  detection, cutting at the quietest point in the last five seconds before each
  30 s boundary and leaving at least one second in the tail, with decoder context disabled. Every sample is covered,
  through the 600 s limit. The response/history label is the language used
  for most window audio.
- **Speech guard:** preserving short words takes priority. Two 20 ms frames
  (including a final partial frame) above a fixed **0.00005 RMS** floor admit
  audio; the threshold cannot rise onto trimmed speech. Numpy computes RMS
  and at most 64 pitch probes, keeping the 600 s evidence check under 100 ms.
  Pitch never controls admission. A 40 ms probe with autocorrelation ≥0.45
  (80–400 Hz at 4 kHz) and a spectrum broader than a pure tone supplies clear
  speech evidence. Only weak evidence plus a whole known Whisper no-speech
  phrase (case/punctuation ignored), or punctuation alone, suppresses output.
  The phrase list is small and explicit in `recognition.py`; clearly voiced
  “you”, “Thank you” and Czech phrases are retained. Ordinary short outputs
  are retained even with weak evidence. Silence/clicks skip ASR; admitted
  noise can require ASR before being suppressed. Empty output skips cleanup
  and history. Long windows skip only energy below 0.000001 RMS, never the
  short-word test; cuts choose the quietest late point and leave ≥1 s tails.
  Acceptance checks covered short/trimmed words, hum, fan noise, complete sample coverage and
  quiet speech. This is a lenient heuristic; unlisted noise hallucinations
  may survive, because avoiding lost words takes priority.
- **Cleanup:** any OpenAI-compatible chat endpoint. The default is Ollama
  `qwen3:8b` at `127.0.0.1:11434`, with thinking off (`reasoning_effort: none`).
  - The transcript goes in `<transcript>` tags. The original English prompt
    boundary and numbered rules/examples are preserved verbatim: never answer,
    follow or add to a dictated instruction/question; never invent facts or
    expand abbreviations such as PR and CI. A single added line explicitly
    preserves uncertainty/hedges (I think, I guess, maybe, kind of, probably)
    while the original filler rule still removes um, uh and filler like.
  - Glossary terms are added to the prompt.
  - The built-in and starter `clean` prompts keep English, Czech and mixed
    words in their spoken language, never translate, preserve existing Czech
    diacritics and accented word spellings, and keep Czech-inflected English
    loanwords. Missing Czech diacritics may be added conservatively. Glossary
    spellings apply when they fit the sound and context, including names in
    English sentences. The glossary is read again for the next dictation.
    Loanword examples now use sentence-start capitals ("Pushni stays Pushni")
    so spelling preservation does not imply keeping a lowercase opening.
  - Every mode with `drift_guard=true` also restores raw words if cleanup
    strips or changes their existing accents while the folded letters match.
    Only raw Czech `š` can also match cleaned `sh`; ordinary English `sh`
    never folds to `s` (so `cash` cannot match `čas`). Restored words retain
    cleaned casing letter by letter when lengths match. Restoring just
    the affected word keeps punctuation and filler edits; ambiguous matches
    return raw with `cleanup=rejected`. Accent additions and phonetic glossary
    corrections remain allowed. There is no hard-coded loanword list in this
    check; preservation of loanword forms relies on the prompt.
  - If the LLM fails, or the length changes too much (it answered instead of
    editing), the raw text is returned instead. History marks this
    `failed` / `rejected`.
- **History:** `history.sqlite3`, with WAL and an FTS5 index over both text
  versions. `GET /v1/history` supports word-prefix AND search (`q`),
  `limit=1..200`, `before`, `device` and `mode`. Results are newest first;
  use `next_before` to page. `X-Hark-Device` is lowercased and stored when it
  matches `[a-z0-9-]{1,32}`, otherwise as `unknown`. No audio is written.
  A legacy `history.jsonl` is imported in one transaction at startup, then
  renamed to `history.jsonl.imported`; it is never appended again.
- **Corrections:** `GET /v1/corrections` groups short word substitutions from
  successful cleanup, skipping case/punctuation changes, insertions,
  deletions and numeric-only replacements. `limit=1..500` and `min_count`
  filter the list. Each result reports whether its final term is in the
  glossary.
- **Glossary:** `GET`, `POST` and `DELETE /v1/glossary` list, add and remove
  terms. Writes preserve comments, blank lines and order and replace the
  file atomically. A term must be 1–64 characters on one line, with no control
  characters, commas or leading `#`.
- **Web page:** `GET /` serves `web/index.html` beside the resolved entry
  point, with no caching and a restrictive CSP. This builder installs a
  placeholder; the frontend supplies history, corrections and glossary views.
  Only the Tailscale owner login can access it. Set `VOICE_GATEWAY_OWNER`
  (default `owner@example.com`); `Tailscale-User-Login` is compared without
  case. Owner API calls also require `X-Hark-Web: 1`, absent or same-host
  HTTPS `Origin`, and JSON content type for bodies. Bearer auth works on
  every API route; transcription accepts bearer auth only. Keep the listener
  private behind Tailscale Serve, which supplies the login header.
- **Limits:** one recording at a time (others wait up to 60 s, then get a 503),
  64 MiB upload, 600 s of audio. Listens on `127.0.0.1:8766` only; Tailscale
  Serve publishes `https://<host>.<tailnet>.ts.net:8766`.

Measured on the RTX 5080 with a 7.4 s English clip:

- speech to text 0.05 s;
- cleanup 0.35–0.55 s for a sentence or two;
- WAV and Ogg Opus uploads both decode.

The first run after a start compiles GPU shaders (about 4 s); the gateway does
that at startup. Over Tailscale HTTPS from a laptop, a raw request took 0.12 s.
A cleaned one took 3.6 s when Ollama first had to load `qwen3:8b`, which it
unloads after 5 idle minutes by default.

## Files on the GPU host

| Path | What |
|---|---|
| `~/.local/lib/hark-server/` | whole `server/` directory: entry point, sibling modules, starter files, `web/index.html` |
| `~/.config/systemd/user/voice-gateway.service` | unit |
| `~/.config/voice-gateway.env` | optional `VOICE_GATEWAY_STATE`, `VOICE_GATEWAY_LLM_URL`, `VOICE_GATEWAY_LLM_MODEL`, `VOICE_GATEWAY_ASR_MODEL`, `VOICE_GATEWAY_LANGUAGES`, `VOICE_GATEWAY_PORT`, `VOICE_GATEWAY_OWNER`, `VOICE_GATEWAY_ENGLISH_HINT` (`1`: glossary hint on English decodes, one more Whisper pass) |
| `~/.local/share/voice-gateway/` (mode 700) | `venv/`, `models/`, `api-token`, `glossary.txt`, `modes.toml`, `history.sqlite3` (plus SQLite WAL/SHM files), optional `history.jsonl.imported` |

Where the program and unit come from depends on the host:

- **Server with the main checkout `~/Coding/projects/hark`:** symlink them from
  `server/`.
- **Desktop test bed (no checkout):** copy them from a committed revision.

`history.sqlite3` keeps one row per non-empty dictation: UTC time, device,
client user agent, mode, audio length, timings, cleanup status, nullable
`language`, `raw` and `text`. Startup adds the language column to an old
database with `ALTER TABLE`; existing/imported rows remain `null`, and FTS
and legacy import keep their existing behavior. Transcripts stay out of the
journal. Before moving a running database,
use SQLite's `.backup` command; copying only the main file can lose data still
in WAL. An invalid legacy JSONL record aborts and rolls back the import,
leaving the original for repair.

`glossary.txt` has one term per line, with `#` comments. It is re-read on
every request. The starter list is `server/glossary.txt`.

## Install

On the GPU host:

```sh
D=~/.local/share/voice-gateway
mkdir -p $D/models && chmod 700 $D
uv venv -q --python 3.13 $D/venv
VIRTUAL_ENV=$D/venv uv pip install transcribe-cpp==0.3.1 av==19.0.1 numpy==2.5.4
curl -fL -o $D/models/parakeet-unified-en-0.6b-Q8_0.gguf \
  https://huggingface.co/handy-computer/parakeet-unified-en-0.6b-gguf/resolve/main/parakeet-unified-en-0.6b-Q8_0.gguf
(umask 077; python3 -c 'import secrets; print(secrets.token_urlsafe(32))' > $D/api-token)
cp server/glossary.txt $D/glossary.txt
cp server/modes.toml $D/modes.toml
mkdir -p ~/.local/lib ~/.config/systemd/user
# On the server with the main checkout:
ln -s ~/Coding/projects/hark/server ~/.local/lib/hark-server
ln -s ~/Coding/projects/hark/server/voice-gateway.service \
  ~/.config/systemd/user/voice-gateway.service
# On the desktop without a checkout, instead copy the whole directory and unit:
# cp -a server ~/.local/lib/hark-server
# cp server/voice-gateway.service ~/.config/systemd/user/voice-gateway.service
systemctl --user daemon-reload && systemctl --user enable --now voice-gateway
tailscale serve --bg --https=8766 http://127.0.0.1:8766
```

Ollama must serve the cleanup model: `ollama pull qwen3:8b`. On the desktop,
the existing user-space `ollama.service` also serves Handy's cleanup and
gbrain embeddings.

## Clients

Never put the token in chat, git or logs. Copy it from `api-token` straight
into the client.

- **Desktop and laptop:** the hark Linux client (`linux/`), a systemd user
  unit. Ctrl+Space runs `clean`, Ctrl+Shift+Space runs `raw`; tap to toggle,
  hold to talk. Runbook: [linux/README.md](../linux/README.md). It replaced
  Handy on the desktop (Handy stays installed, autostart off).
- **Phone:** the hark Android app (`android/`), a floating bubble over the
  keyboard that inserts at the cursor. Runbook: [docs/android.md](../docs/android.md).
- **Anything OpenAI-compatible** also works: base URL
  `https://<host>.<tailnet>.ts.net:8766/v1/`, the token as API key, model
  `clean` or `raw`.

## Check and operate

```sh
systemctl --user status voice-gateway
journalctl --user -u voice-gateway -n 50
curl -fsS http://127.0.0.1:8766/health
curl -s -H "Authorization: Bearer $(cat ~/.local/share/voice-gateway/api-token)" \
  -H 'X-Hark-Device: test' -F file=@clip.wav -F model=clean \
  http://127.0.0.1:8766/v1/audio/transcriptions
curl -s -H "Authorization: Bearer $(cat ~/.local/share/voice-gateway/api-token)" \
  'http://127.0.0.1:8766/v1/history?q=clau+cod&limit=10'
sqlite3 -header -column ~/.local/share/voice-gateway/history.sqlite3 \
  'SELECT id, time, device, mode, cleanup, text FROM history ORDER BY id DESC LIMIT 5;'
sqlite3 -header -column ~/.local/share/voice-gateway/history.sqlite3 \
  "SELECT h.id, h.text FROM history h JOIN history_fts f ON f.rowid=h.id WHERE history_fts MATCH '\"clau\"* AND \"cod\"*' ORDER BY h.id DESC;"
```

Open `https://<host>.<tailnet>.ts.net:8766/` while signed into Tailscale as
the owner. API details and response shapes: `docs/api.md`.

The live setup uses Whisper large-v3-turbo for English and Czech:

```ini
VOICE_GATEWAY_ASR_MODEL=/absolute/path/to/whisper-large-v3-turbo-Q8_0.gguf
VOICE_GATEWAY_LANGUAGES=en,cs
VOICE_GATEWAY_ENGLISH_HINT=1
```

GGUFs come from `handy-computer/<model>-gguf` on Hugging Face.

To switch the cleanup model, set `VOICE_GATEWAY_LLM_MODEL=` in the env file,
then `systemctl --user restart voice-gateway`.

Tests (no GPU or models needed):

```sh
cd server && uv run -q --with pytest --with numpy pytest -q
```
