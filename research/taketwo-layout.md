# TakeTwo layout: engine, window and MCP server

Research for issue #6. Date: 2026-11-26. Nothing was installed or built. Each claim names its source.
"Today" means the current VoiceForge code in this repo.

## Recommendation

Ship **three Rust binaries**. The engine is its own process, and it keeps today's local HTTP API as the only way in.

```
  Re:Create, Media Machine,        Claude / other MCP clients
  curl, old Python MCP                      |
          |                                 | stdio (MCP)
          |                          +--------------+
          |                          | taketwo-mcp  |  small rmcp binary
          |                          +--------------+
          |                                 |
          |   reads engine.json  <----------+  (starts the engine if none answers)
          v                                 v
  +-----------------------------------------------------------+
  | taketwo-engine  (separate process, 127.0.0.1:8766-8785)   |
  |  axum HTTP API  /api/...  + serves the React UI           |
  |  job queue  -> ONE GPU worker thread (owns the models)    |
  |  writes engine.json, holds engine.lock                    |
  +-----------------------------------------------------------+
          ^                                 ^
          | HTTP (relative /api fetches)    | spawns sidecar / attaches
  +-------------------+            +---------------------------+
  | Tauri webview     |  <-------- | taketwo (Tauri shell)     |
  | loads engine URL  |            | menus, single instance,   |
  +-------------------+            | quit confirmation         |
                                   +---------------------------+
```

1. **`taketwo-engine`**: an axum server plus one dedicated GPU worker thread. It is bundled as a Tauri sidecar and runs as its own process.
2. **`taketwo`**: the Tauri shell. It starts the engine or attaches to one that is already running. Its window then loads the engine's own address, just as Electron does today.
3. **`taketwo-mcp`**: a small `rmcp` stdio server. It finds the engine through `engine.json`, starts it if needed, and calls the HTTP API.

This layout means no new API surface. The React code, the `only_this_computer` checks and every existing caller stay as they are.

## 1. Engine in the Tauri process or a separate sidecar

**Pick: a separate sidecar process.**

- **The engine must work without the window.** The MCP server and other apps call the HTTP API directly (README, "Use it from Claude or your own tools"). Today, when no engine answers, the MCP server opens the app (`mcp/voiceforge_mcp.py`, `_engine()`, `open -g -a VoiceForge`). A separate engine binary can be started headless by the MCP server on both platforms. `open -a` exists only on macOS.
- **Crash isolation.** Today, when the engine dies, the shell shows "The engine stopped unexpectedly" and stays up (`desktop/main.js`, `server.on("exit")`). Inside one process, a GPU abort or out-of-memory kill would take the window down with it. With a separate process, only the engine goes.
- **GPU memory.** Today the engine frees models after idle (`_release_models` in `backend/app/main.py`). A separate process can also return everything to the OS by exiting, which an in-process engine cannot. (Hypothesis: driver-level leaks on Windows/CUDA are only fully cleared by process exit. Not verified.)
- **Tauri supports this.** Tauri bundles extra binaries listed in `bundle.externalBin`. Each one needs a `-$TARGET_TRIPLE` filename suffix, and Rust starts it with `app.shell().sidecar("name").spawn()` ([Tauri: sidecars](https://v2.tauri.app/develop/sidecar/)). The returned `CommandChild` has `kill()` and `pid()`. It is killed on drop only if `kill_on_drop` is set, and with `process_group` its whole process group or Windows job object is killed ([tauri-plugin-shell CommandChild](https://docs.rs/tauri-plugin-shell/latest/tauri_plugin_shell/process/struct.CommandChild.html)).

**Only one engine at a time.** The engine takes an exclusive `File::try_lock` on `engine.lock` in the address folder. If the lock fails, another engine is running, so it exits.
- `try_lock` is stable since Rust 1.89. It maps to `flock` on Unix and `LockFileEx` on Windows, and the lock is released when the file is closed ([std::fs::File](https://doc.rust-lang.org/std/fs/struct.File.html)).
- Only after binding the port does the engine write `engine.json` (atomically, as today: `url`, `pid`, `started`).

**Who starts the engine:**
- **The window.** It reads `engine.json` and checks the engine answers as TakeTwo (as `desktop/main.js` does today). If not, it spawns the sidecar.
- **The MCP server.** Same check. If no engine answers, it starts `taketwo-engine` from the folder next to its own executable. This replaces `open -g -a`.
- **Single instance.** The Tauri single-instance plugin stops a second window instance. Its callback receives the new launch's `args` and `cwd`, it must be registered first, and it needs no JS capabilities ([Tauri: single instance](https://v2.tauri.app/plugin/single-instance/)).

**On quit,** keep today's rule: confirm when a job is running (`desktop/main.js`, `confirmQuit`). Then the shell explicitly asks the engine to stop or kills it. Neither Tauri page says whether a sidecar dies with the app (see "Not verified"), so do not rely on that.

**Open decision:** whether an engine that the MCP server started headless exits after some idle time, or stays up. Today the engine lives only as long as the window.

## 2. How the React UI talks to the engine

**Pick: keep the local HTTP API, and let the Tauri window load the engine's address.**

- **Today's UI.** The UI uses relative `fetch` calls through `req()` in `frontend/src/api.ts`. The engine serves it, and Electron loads it with `win.loadURL(ORIGIN)` (`desktop/main.js`).
- **Tauri can do the same.** `WebviewUrl::External(Url)` is "An external URL. Must use either the `http` or `https` schemes" ([tauri WebviewUrl](https://docs.rs/tauri/latest/tauri/enum.WebviewUrl.html)). The page origin stays `http://127.0.0.1:<port>`. The existing Origin rule, "a web page may change things only if this engine served it", then passes unchanged (`only_this_computer`, `backend/app/main.py`).
- **Why not bundle the UI with Tauri commands?** The UI would then load from Tauri's own origin, which is `http://tauri.localhost` on Windows according to the config reference example ([Tauri config reference](https://v2.tauri.app/reference/config/)). The current middleware refuses writes from that origin. More importantly, MCP and the other apps still need HTTP, so the project would maintain two APIs.
  - Tauri commands are fine for UI-only needs: async commands run on a separate task, and Channels are recommended for streaming ([Tauri: calling Rust](https://v2.tauri.app/develop/calling-rust/)). But they would only reach the window.
  - By default, remote pages cannot call Tauri commands. A capability must list them under `remote.urls` ([Tauri: capabilities](https://v2.tauri.app/security/capabilities/)). So the UI should not depend on Tauri commands at all.
  - Native parts (menus, quit confirmation, single instance) live in the Rust shell. The shell calls the engine over HTTP like any other client.
- **Keeping existing callers working.**
  - Port the HTTP API one to one: same paths, JSON shapes and status values, including `outcome`/`review` on `GET /api/generations/<id>`.
  - Keep the 8766-8785 port range. Old callers scan it, for example `PORTS` in `mcp/voiceforge_mcp.py`.
  - Keep the Host and Origin checks and the id validation (`_item_dir`).
  - FastAPI's `/docs` OpenAPI schema is today's contract. Snapshot it before the rewrite and test the Rust engine against it.

## 3. The MCP server in Rust

**Pick: a separate `taketwo-mcp` binary using `rmcp` over stdio.**

- **The SDK.** `rmcp` is "The official Rust SDK for the Model Context Protocol" ([modelcontextprotocol/rust-sdk](https://github.com/modelcontextprotocol/rust-sdk)).
  - Stdio is the `transport-io` feature.
  - `#[tool_router(server_handler)]` with `#[tool(description = ...)]` and `Parameters<T>` (`T: JsonSchema`) builds a tools-only server.
  - The SDK also has `transport-streamable-http-server`, which "Exposes a Tower service you can mount on any router" such as axum.
- **Why stdio and not `/mcp` inside the engine.** Today's setup is `claude mcp add ... voiceforge -- <command>` over stdio (README). A stdio binary can start the engine on demand. An HTTP endpoint needs the engine already running at a port that changes. Mounting `/mcp` in the engine later stays cheap if it's ever needed.
- **Keep the same five tools:** `list_voices`, `generate_narration`, `get_status`, `get_timings`, `get_audio`.
- **Finding the engine, in order:**
  1. the `TAKETWO_URL` / `VOICEFORGE_URL` environment variable;
  2. `engine.json` under the TakeTwo folder, then the VoiceForge folder;
  3. a scan of 8766-8785 that accepts only an engine that identifies itself (as `_is_voiceforge` does today);
  4. starting `taketwo-engine` from next to its own executable and waiting.
- **Transition.** Register the same binary under both MCP names, `taketwo` and `voiceforge`.

## 4. Long GPU jobs, cancellation, one queue

**Pick: one dedicated OS thread that owns the models and takes jobs from a channel. This is what `backend/app/worker.py` does today.**

- **Off the async runtime.** Tokio warns that "issuing a blocking call or performing a lot of compute in a future without yielding is problematic". `spawn_blocking` moves such work to a blocking thread pool. But those tasks "cannot be aborted" ([tokio spawn_blocking](https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html)), and a pool would let two GPU jobs run at once. A single `std::thread` fed by a queue keeps one job on the GPU at a time.
- **Cancellation stays cooperative,** as today: a per-job flag that the worker checks between chunks (`_cancel_requested` in `worker.py`). Queued jobs are removed before they start (`cancel()` and `_on_cancel`).
- **The window never freezes.** The GPU runs in another process, and the UI only makes HTTP requests. Even inside one process, the work would sit on its own thread, not on the UI thread.
- **Progress.** Polling works today, so keep it. axum supports Server-Sent Events (`Sse`, `Event`, `KeepAlive`) if live progress is wanted later ([axum sse](https://docs.rs/axum/latest/axum/response/sse/index.html)). That is optional, not required.
- **Interrupted jobs.** Keep today's "interrupted, press Resume" handling on start (`backend/app/main.py`).

## 5. Where the address file and library live

Use `directories::BaseDirs` plus a fixed folder name. Do **not** use `ProjectDirs` or Tauri's `app_data_dir`:
- `ProjectDirs` adds `\data` or `\config` on Windows and a qualified project path on macOS ([directories ProjectDirs](https://docs.rs/directories/latest/directories/struct.ProjectDirs.html)).
- Tauri's `app_data_dir` appends the bundle identifier ([tauri PathResolver](https://docs.rs/tauri/latest/tauri/path/struct.PathResolver.html)).

Neither gives the plain path that other apps already compute.

`BaseDirs::data_local_dir` is `$HOME/Library/Application Support` on macOS and `{FOLDERID_LocalAppData}` (for example `C:\Users\Alice\AppData\Local`) on Windows. `data_dir` is the Roaming folder on Windows ([directories BaseDirs](https://docs.rs/directories/latest/directories/struct.BaseDirs.html)).

| What | macOS | Windows |
|---|---|---|
| Address file (`engine.json`, `engine.lock`) | `~/Library/Application Support/TakeTwo/` and a copy of `engine.json` in `.../VoiceForge/` | `%LOCALAPPDATA%\TakeTwo\` and `%LOCALAPPDATA%\VoiceForge\` |
| Library (voices, narrations, database, settings) | `~/Library/Application Support/TakeTwo/library/` | `%LOCALAPPDATA%\TakeTwo\library\` |
| Downloaded models | `~/Library/Application Support/TakeTwo/models/` (or `~/Library/Caches`, decide later) | `%LOCALAPPDATA%\TakeTwo\models\` |
| Override | `TAKETWO_DATA` (also accept `VOICEFORGE_DATA`) | same |

- **Why Local and not Roaming on Windows.** Local "does not get copied if the user profile roams", and "Caches and similar non-essential data should be kept here, especially if they are large" ([Microsoft, The Old New Thing](https://devblogs.microsoft.com/oldnewthing/20050701-09/?p=35133)). The library holds gigabytes of audio and models. An engine address only makes sense on one machine.
- **Same path on macOS.** `data_local_dir` returns today's `~/Library/Application Support`, so the VoiceForge address file stays exactly where current callers read it (`ENGINE_FILE` in `backend/app/main.py`, `_engine()` in `mcp/voiceforge_mcp.py`).
- **Library location today.** It is the repo's `data/` folder (`DATA_DIR` in `backend/app/config.py`). Moving it to the app data folder needs a one-time migration step. That is outside this ticket.

## Risks

1. **An orphaned engine after a crash or force-quit.** No source checked says a Tauri sidecar dies with the app. Mitigation: the lock file plus attach-on-start means an orphan gets reused, not duplicated. On Windows, use `process_group` / job object.
2. **Two engines racing for the port.** The window and the MCP server could start engines at the same moment. The lock must be taken before the bind, and the loser must read `engine.json` and wait for it.
3. **Lock behaviour.** It is advisory and "may change in the future" (std docs). Release on process exit is inferred from "released when this file is closed", not stated.
4. **The API is unauthenticated on localhost.** Any local process can call it, as today. The Host and Origin checks must be ported exactly, with tests. Loading the UI from `127.0.0.1` relies on those checks, not on Tauri's capability model.
5. **Sidecar naming and signing.** Each binary needs a target-triple copy per platform (Tauri sidecar docs). macOS signing/notarisation and Windows SmartScreen apply to all three binaries.
6. **Size limit.** Three binaries share the 100 MB budget. GPU libraries (especially CUDA on Windows) may not fit unless they are also downloaded on first use. Not measured.
7. **WebView differences.** Tauri uses WebKit on macOS and WebView2 on Windows, while Electron uses Chromium. Audio playback and recording in the UI must be retested on both.
8. **Keeping the old name.** Two address files can disagree if only one gets written. Write both from the same code path, in one step.

## Not verified

- The current `rmcp` version number (the README mentions a 3.x migration guide, but no version) and its MSRV.
- Whether the Tauri shell plugin kills sidecars on app exit, and the default of `kill_on_drop`.
- Whether a page loaded with `WebviewUrl::External` from `http://127.0.0.1` has quirks on WebView2 or WKWebView: mixed content, media permissions, or the microphone for recording.
- Windows GPU behaviour: CUDA memory release, driver crashes, and which Rust ML runtime is used. That belongs to a separate research ticket.
- Binary sizes against the 100 MB limit.
- Microsoft's current Learn page on Roaming vs Local. The cited source is a Microsoft blog post from 2005.
