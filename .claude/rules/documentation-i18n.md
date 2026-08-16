---
# Path-scoped rule — applies only when Claude works with Markdown docs.
# Target extension: .md (every directory's README.md + the docs tree).
# Generated/vendored Markdown is excluded (see "Out of scope" below).
paths:
  - "*.md"
  - "ai-docs/**/*.md"
  - "ai-docs/**/*.ja.md"
---

# Documentation rules — bilingual en / ja

**English is canonical; Japanese is a translation that may lag.**

## Every doc has both languages (`*.ja.md` suffix)

- Each Markdown doc has an English source and a Japanese sibling using the
  `.ja.md` suffix: `foo.md` ⇆ `foo.ja.md` (e.g. `README.md` ⇆ `README.ja.md`,
  `0001-tech-stack.md` ⇆ `0001-tech-stack.ja.md`).
- When you **create or edit** an English doc, create/update its `.ja.md`
  sibling in the same change (and vice-versa). If you genuinely cannot translate
  now, say so explicitly — do not leave a language silently missing or stale.
- Keep identical filenames and numbering; only the `.ja` suffix differs.

## Language switcher = first content line of every doc

- English file: `> 🌐 **English** · [日本語](<name>.ja.md)`
- Japanese file:

  ```markdown
  > 🌐 [English](<name>.md) · **日本語**
  > _英語版が正(canonical)です。差分がある場合は英語版を参照してください。_
  ```

## Links stay within one language

- A `.ja.md` file links to other `.ja.md` files; an English file links to
  English files. Never cross languages in body links (the switcher is the only
  cross-language link).
- Use relative links. Keep code blocks, commands, paths, and identifiers in
  English in both versions; translate prose only.

## Out of scope (do NOT create `.ja.md` for these)

- Open Knowledge Format (OKF): `**/index.md`, `**/log.md`.
- Vendored / build output: `node_modules/`, `dist/`, `.next/`, `build/` — and
  anything else listed in `.gitignore`.
- Claude system prompts: `.claude/**/*.md`.
