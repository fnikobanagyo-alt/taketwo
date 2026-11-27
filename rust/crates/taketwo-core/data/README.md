# Embedded text data (read with `include_str!`, so tests need no model folder)

- `whisper_normalizer.json`: the British to American spelling map, an unchanged copy of
  `normalizer.json` from the `openai/whisper-large-v3-turbo` tokenizer files (the same list
  ships as `whisper/normalizers/english.json` in github.com/openai/whisper). OpenAI publishes
  both under the MIT licence. The local snapshot holds no licence file, so this was taken from
  the published model card and repository, not re-checked offline. `rust/golden/record.py`
  asserts the tokenizer's map still equals this file.
- `html5_entities.json`: Python 3.11's `html.entities.html5` table (Python Software Foundation
  licence; the entity names come from the WHATWG HTML standard), dumped with
  `json.dump(html.entities.html5, f, ensure_ascii=False, sort_keys=True, indent=0)`.
