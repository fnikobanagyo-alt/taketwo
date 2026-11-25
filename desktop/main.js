// VoiceForge desktop shell: owns the window, starts/stops the local engine.
const { app, BrowserWindow, Menu, dialog, nativeTheme, session, shell, systemPreferences } = require("electron");
const { spawn } = require("child_process");
const fs = require("fs");
const http = require("http");
const net = require("net");
const os = require("os");
const path = require("path");

// 8765 and 8766 are often held by other local servers. The engine takes the first free port from here.
const PREFERRED_PORT = 8766;
let PORT = PREFERRED_PORT;
let ORIGIN = `http://127.0.0.1:${PORT}`;
const CONFIG = JSON.parse(fs.readFileSync(path.join(__dirname, "config.json"), "utf8"));
const ROOT = CONFIG.root; // the voiceforge project folder (backend, frontend, data)
const LOG_DIR = path.join(os.homedir(), "Library", "Logs", "VoiceForge");

let win = null;
let server = null; // child process we started (null if we attached to a running one)
let quitting = false;

if (!app.requestSingleInstanceLock()) app.quit();
app.on("second-instance", () => { if (win) { if (win.isMinimized()) win.restore(); win.focus(); } });

function get(pathname, timeout = 1500) {
  return new Promise((resolve) => {
    const req = http.get(ORIGIN + pathname, { timeout }, (res) => {
      let body = "";
      res.on("data", (d) => (body += d));
      res.on("end", () => { try { resolve({ ok: res.statusCode === 200, json: JSON.parse(body) }); } catch { resolve({ ok: res.statusCode === 200 }); } });
    });
    req.on("error", () => resolve({ ok: false }));
    req.on("timeout", () => { req.destroy(); resolve({ ok: false }); });
  });
}

function portFree(port) {
  return new Promise((resolve) => {
    const probe = net.createServer();
    probe.once("error", () => resolve(false));
    probe.once("listening", () => probe.close(() => resolve(true)));
    probe.listen(port, "127.0.0.1");
  });
}

async function choosePort() {
  for (let p = PREFERRED_PORT; p < PREFERRED_PORT + 20; p++) if (await portFree(p)) return p;
  throw new Error("No free local port from 8766 to 8785. Close other local servers, then start VoiceForge again.");
}

// Only VoiceForge's own engine counts: another server answering 200 on the same port must not pass.
async function engineUp() {
  const r = await get("/api/health");
  return r.ok && r.json?.ok === true;
}

function splash(status, error = false) {
  if (win && !win.isDestroyed()) win.webContents.executeJavaScript(`window.setStatus && window.setStatus(${JSON.stringify(status)}, ${error})`).catch(() => {});
}

function startServer() {
  const py = path.join(ROOT, "backend", ".venv", "bin", "python");
  if (!fs.existsSync(py)) throw new Error(`Engine not installed at ${ROOT}. Run start.sh once to set it up.`);
  if (!fs.existsSync(path.join(ROOT, "frontend", "dist", "index.html"))) throw new Error("Interface not built. Run start.sh once.");
  fs.mkdirSync(LOG_DIR, { recursive: true });
  const log = fs.openSync(path.join(LOG_DIR, "engine.log"), "a");
  const env = {
    ...process.env,
    PATH: [path.join(os.homedir(), ".local", "bin"), "/opt/homebrew/bin", "/usr/local/bin", process.env.PATH || "/usr/bin:/bin"].join(":"),
    PYTHONUNBUFFERED: "1",
  };
  server = spawn(py, ["-m", "uvicorn", "app.main:app", "--host", "127.0.0.1", "--port", String(PORT), "--log-level", "warning"], {
    cwd: path.join(ROOT, "backend"), env, stdio: ["ignore", log, log],
  });
  server.on("exit", (code) => {
    server = null;
    if (!quitting) splash(`The engine stopped unexpectedly (code ${code}). See ~/Library/Logs/VoiceForge/engine.log`, true);
  });
}

async function boot() {
  let up = await engineUp();
  if (!up) {
    splash("Starting the voice engine…");
    try {
      PORT = await choosePort();
      ORIGIN = `http://127.0.0.1:${PORT}`;
      startServer();
    } catch (e) { splash(e.message, true); return; }
    for (let i = 0; i < 120 && !up; i++) {
      await new Promise((r) => setTimeout(r, 500));
      up = await engineUp();
      if (i === 10) splash("Loading audio libraries…");
      if (i === 40) splash("Almost there…");
    }
  }
  if (!up) return splash("The engine didn't start. See ~/Library/Logs/VoiceForge/engine.log", true);
  await win.loadURL(`${ORIGIN}/#/home`);
}

function createWindow() {
  win = new BrowserWindow({
    width: 1440, height: 920, minWidth: 1100, minHeight: 700,
    titleBarStyle: "hiddenInset", trafficLightPosition: { x: 18, y: 18 },
    vibrancy: "sidebar", visualEffectState: "followWindow", backgroundColor: "#00000000", transparent: false,
    show: false, title: "VoiceForge",
    webPreferences: { preload: path.join(__dirname, "preload.js"), contextIsolation: true, sandbox: true, spellcheck: true },
  });
  win.loadFile(path.join(__dirname, "splash.html"));
  win.once("ready-to-show", () => win.show());
  win.webContents.once("did-finish-load", boot);

  // external links -> default browser; keep the app on our origin
  win.webContents.setWindowOpenHandler(({ url }) => { if (!url.startsWith(ORIGIN)) shell.openExternal(url); return { action: "deny" }; });
  win.webContents.on("will-navigate", (e, url) => {
    if (!url.startsWith(ORIGIN) && !url.startsWith("file://")) { e.preventDefault(); shell.openExternal(url); }
  });
  win.on("close", async (e) => {
    if (quitting) return;
    e.preventDefault();
    if (await confirmQuit()) { quitting = true; app.quit(); }
  });
}

async function confirmQuit() {
  const st = await get("/api/status");
  const r = st.json && st.json.running;
  if (!r || !server) return true;
  const { response } = await dialog.showMessageBox(win, {
    type: "warning", buttons: ["Quit", "Keep working"], defaultId: 1, cancelId: 1,
    message: "A job is still running",
    detail: `${r.label}: ${r.message || ""}\n\nIf you quit now it pauses. Open VoiceForge again and press Resume in History to finish it.`,
  });
  return response === 0;
}

function menu() {
  const tpl = [
    { role: "appMenu" },
    { role: "fileMenu" },
    { role: "editMenu" },
    {
      label: "View",
      submenu: [
        { label: "Home", accelerator: "CmdOrCtrl+1", click: () => win && win.loadURL(`${ORIGIN}/#/home`) },
        { label: "Text to Speech", accelerator: "CmdOrCtrl+2", click: () => win && win.loadURL(`${ORIGIN}/#/speech`) },
        { label: "Voices", accelerator: "CmdOrCtrl+3", click: () => win && win.loadURL(`${ORIGIN}/#/voices`) },
        { label: "History", accelerator: "CmdOrCtrl+4", click: () => win && win.loadURL(`${ORIGIN}/#/history`) },
        { type: "separator" },
        { role: "reload" }, { role: "toggleDevTools" }, { type: "separator" },
        { role: "resetZoom" }, { role: "zoomIn" }, { role: "zoomOut" }, { type: "separator" }, { role: "togglefullscreen" },
      ],
    },
    { role: "windowMenu" },
    { role: "help", submenu: [{ label: "Open data folder", click: () => shell.openPath(path.join(ROOT, "data")) }, { label: "Open engine log", click: () => shell.openPath(path.join(LOG_DIR, "engine.log")) }] },
  ];
  Menu.setApplicationMenu(Menu.buildFromTemplate(tpl));
}

app.whenReady().then(async () => {
  nativeTheme.themeSource = "system";
  menu();
  // microphone for in-app recording (local origin only)
  session.defaultSession.setPermissionRequestHandler((wc, perm, cb) => cb(perm === "media" && wc.getURL().startsWith(ORIGIN)));
  session.defaultSession.setPermissionCheckHandler((wc, perm, origin) => perm === "media" && (origin || "").startsWith(ORIGIN));
  if (systemPreferences.getMediaAccessStatus("microphone") === "not-determined") systemPreferences.askForMediaAccess("microphone").catch(() => {});
  // Downloads go straight to ~/Downloads/VoiceForge and are revealed (highlighted) in Finder.
  session.defaultSession.on("will-download", (_e, item) => {
    const dir = path.join(app.getPath("downloads"), "VoiceForge");
    fs.mkdirSync(dir, { recursive: true });
    const ext = path.extname(item.getFilename());
    const base = path.basename(item.getFilename(), ext);
    let target = path.join(dir, item.getFilename());
    for (let n = 2; fs.existsSync(target); n++) target = path.join(dir, `${base} (${n})${ext}`);
    item.setSavePath(target);
    item.once("done", (_ev, state) => {
      if (state === "completed") {
        shell.showItemInFolder(target);
        if (win && !win.isDestroyed()) win.webContents.executeJavaScript(`window.dispatchEvent(new CustomEvent("vf-downloaded", { detail: ${JSON.stringify(path.basename(target))} }))`).catch(() => {});
      } else if (win && !win.isDestroyed()) {
        dialog.showMessageBox(win, { type: "error", message: "Download failed", detail: `${item.getFilename()} could not be saved (${state}).` });
      }
    });
  });
  createWindow();
});

app.on("activate", () => { if (BrowserWindow.getAllWindows().length === 0) createWindow(); });
app.on("window-all-closed", () => app.quit());
app.on("before-quit", async (e) => {
  if (!quitting) {
    e.preventDefault();
    if (await confirmQuit()) { quitting = true; app.quit(); }
    return;
  }
  if (server) { try { server.kill("SIGTERM"); } catch { /* already gone */ } }
});
