# TakeTwo

_Formerly called VoiceForge._

A private voice-cloning studio that runs entirely on your Mac. It focuses on one thing: long narration
that sounds like the cloned voice, with every part checked for dropouts, cut-off endings and wrong words.
Inspired by [Voicebox](https://github.com/jamiepine/voicebox).

> **Use it responsibly.** Clone only your own voice, or a voice whose owner has given you permission. Don't
> use it to impersonate, deceive or harass anyone. Follow the laws where you live and label synthetic speech
> where that is expected.

## What you need
- A Mac with **Apple Silicon** (M1 or newer), **macOS 14+**. Developed on an M4 Pro with 24 GB of memory; the models use about 6 GB.
- About **10 GB of free disk space**: roughly 1 GB of Python packages and 6.2 GB of models (Qwen3-TTS 4.3 GB,
  Whisper 1.5 GB, Sidon 0.5 GB), plus room for recordings. Models download from Hugging Face the first time
  they're needed: about 2 GB for your first voice, the rest for your first narration. After that it works offline.
- [Homebrew](https://brew.sh), then:

```bash
brew install python@3.11 ffmpeg node
```

## Install and start

```bash
git clone https://github.com/fnikobanagyo-alt/taketwo.git
cd taketwo
./start.sh
```

The first run takes several minutes. Your browser opens when it's ready. Later runs start in seconds and
pick up updates after `git pull`. To check the requirements without installing anything: `./start.sh --check`.

**Optional desktop app.** It uses this folder's engine, so keep the folder where it is:

```bash
./start.sh --setup
cd desktop && npm ci && npm run dist
ditto dist/mac-arm64/TakeTwo.app /Applications/TakeTwo.app
```

## Your first narration
1. **Voices → Clone a voice.** Add 1 to 5 minutes of one person speaking, in a quiet room.
2. **Text to Speech.** Choose the voice, paste a script (up to 10,000 words) and press Generate.
3. **Listen.** A narration ends as *Done*, or *Done, needs a listen* with the doubtful parts listed. The checks
   are good but not perfect, so listen before publishing. Any part can be re-spoken on its own.
4. **Export** WAV, MP3, SRT, VTT or the script.

## Tips for a close clone
- One speaker, no music, steady microphone distance.
- Pick the reference clip whose delivery matches what you want.
- Correct names Whisper misheard in the reference transcript.
- Add hard names in **Settings → Pronunciation fixes**.

## Models
| Model | Notes |
|---|---|
| Qwen3-TTS 1.7B (default) | Best all-round cloning, Apache-2.0 |
| VoxCPM2 | 48 kHz, Apache-2.0 |
| Qwen3-TTS 0.6B | Faster |
| Chatterbox / Multilingual | Very stable, MIT |
| Fish Audio S2 Pro | Expressive, **non-commercial** licence |
| OmniVoice | Fixed length, Apache-2.0 |

Model licences are separate from TakeTwo's. Check the model's page before commercial use.

## Privacy and safety
- Voices, scripts and audio stay in `data/` on your Mac. Nothing is uploaded. Model downloads and the Models
  page contact Hugging Face and PyPI. Once at launch TakeTwo asks GitHub whether a newer release exists; turn
  this off in Settings → Updates. Nothing about you, your voices or your scripts is sent.
- The engine listens only on this computer and refuses changes from other websites. It has no password, so
  any program already running under your account can use it.
- Never publish `data/`. Git ignores it, along with audio files.

## If something goes wrong
| Problem | Try |
|---|---|
| "can't start: … is missing" | Run the command it shows, then `./start.sh` again |
| Install stopped halfway | Run `./start.sh` again; it finishes the job |
| A model download stalls | Press Download again on the Models page; it resumes |
| Very slow rendering | Close memory-hungry apps. TakeTwo frees its models after 5 idle minutes |
| Old settings cause trouble | A damaged file is moved to `data/settings.json.broken` and defaults are used |
| Anything else | Desktop app log: `~/Library/Logs/TakeTwo/engine.log` |

## Updates
When a new release is out, a card in the sidebar offers it. **Update** waits for running work to finish, moves
this copy to the release, reinstalls parts if needed and asks you to restart. Your library is never touched. If
anything fails it stays on the old version. A copy with its own edits, or one not installed with git, is not
updated automatically: run `git pull && ./start.sh` yourself. The desktop app's window shell only changes when
you rebuild it (`cd desktop && npm run dist`).

## Use it from Claude or your own tools
MCP server (needs [uv](https://docs.astral.sh/uv/)):

```bash
claude mcp add --scope user taketwo -- uv run --script "$PWD/mcp/taketwo_mcp.py"
```

Tools: `list_voices`, `generate_narration`, `get_status`, `get_timings`, `get_audio`.

HTTP: read `~/Library/Application Support/TakeTwo/engine.json` for the address; API docs are at `<address>/docs`.
`GET /api/generations/<id>` returns `status`, plus `outcome` (`clean` or `needs_listen`) and `review` (each doubtful
part with its text and reasons) once it's done.

## Development
```bash
backend/.venv/bin/python -B scripts/check.py     # backend tests and interface build, on temporary data
cd frontend && npm run dev                       # live interface, with the engine running
```
`backend/` is FastAPI and MLX, `frontend/` is React, `desktop/` is the Electron shell. `TAKETWO_DATA` points to
another library and `TAKETWO_PORT` forces a port (the old `VOICEFORGE_` names still work).

Publishing a release: set `APP_VERSION` in `backend/app/config.py`, commit, tag it (`git tag v2.1.0`), push the
tag, then publish a GitHub Release for that tag. The first line of its notes shows in the update card.

## Licence
[Apache-2.0](LICENSE). Not affiliated with the model authors.
