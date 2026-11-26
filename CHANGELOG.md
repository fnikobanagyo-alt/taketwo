# Changelog

What changed in each version of TakeTwo. The first line under a version is what the in-app update card shows.

## 2.0.0 (first release, not yet published)

The first TakeTwo release: review every doubtful part by ear, compare takes, and get told about new versions.

### Added
- **Review room.** One waveform of the finished narration with the parts that need review shaded, above one
  list of the script's parts. Each doubtful part says why and what was measured, with **Sounds fine** (undoable),
  **Re-speak** and **Edit words**. Optional keyboard shortcuts: Space, N/P, A, R, E.
- **Earlier takes.** Re-speaking a part keeps the previous take, so Take 1 and Take 2 can be heard side by side
  and either kept; the narration is re-mixed.
- **Studio Desk home**: resume your draft, jump to what needs review, recent scripts grouped with their takes.
- **History** groups takes of the same script, shows where a narration came from (MCP), and has folder buttons
  that show a narration's audio file in Finder or Explorer. Delete can be undone.
- **Composer**: a Title field, a notice when the same script was already narrated in that voice, and a
  collapsible Delivery & tone section.
- **Update check**: once at launch TakeTwo asks GitHub for the latest release and offers it in the sidebar;
  Update waits for running work, installs, and asks before restarting. Can be turned off in Settings.
- **MCP tools**: `respeak_part`, `fix_remaining`, `find_narration`, `stop_narration`, `resume_narration`,
  `get_subtitles`, `mark_sounds_fine`; richer `list_voices`, `get_status` and `get_audio` (`output_path`).
- API: accept/undo per part, take audio and select-take, restore after delete, search, and `source` and
  `title` stored on each narration.

### Changed
- Renamed from **VoiceForge** to **TakeTwo**. The old name keeps working for now: `VOICEFORGE_*` settings,
  the address file under both names, and the `voiceforge` MCP entry.
- New logo, wordmark and fonts (Instrument Serif headings, Instrument Sans text).
- Wording follows the glossary: "Needs review", "Re-speak", "Stopped", "part".

### Accessibility
- Text contrast meets WCAG AA in light and dark mode; no text under 12 px.
- Space no longer steals focused buttons; every icon button, switch and dropdown is labelled; dropdowns work
  with the keyboard; dialogs trap focus; reduced-motion is respected.

## Before TakeTwo (published as VoiceForge, 2026-11-25)

First public version: voice cloning, long narration with every part checked, re-speak, subtitles, MCP server.
