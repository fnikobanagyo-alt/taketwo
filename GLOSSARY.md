# TakeTwo

A local app that clones a voice and reads long scripts aloud in it. Formerly called VoiceForge.

## Language

**TakeTwo**:
This app: the voice-cloning narration studio, on Mac and Windows.
_Avoid_: VoiceForge (its former name)

**Narration**:
One script read aloud in one voice, with its finished audio. It holds the lasting outcome of the work done on it.
_Avoid_: Generation, render, job

**Part**:
A short, sentence-aligned piece of a narration's script, recorded on its own and joined into the finished audio.
_Avoid_: Chunk, segment

**Take**:
One recording attempt of a part. A part may have several takes, and one is kept.
_Avoid_: Attempt, sample

**Re-speak**:
Recording one chosen part again, optionally with new text, while every other part stays as it is.
_Avoid_: Regenerate

**Stopped**:
The state of a narration whose work ended before finishing, whether you pressed Stop or the app closed. Its finished parts are kept and it can be resumed.
_Avoid_: Cancelled, interrupted

**Needs review**:
A part whose kept take failed a quality check, or a narration that has such parts. The narration is still complete and playable.
_Avoid_: Flagged
