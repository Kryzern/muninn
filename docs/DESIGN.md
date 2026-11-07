# Bifröst: the suite's design language

Heimdall guards Bifröst, the rainbow bridge. **Heimdall owns the whole spectrum; every other app owns one band of it.** That one rule ties the suite together without runes, knotwork or fantasy styling.

## The spectrum

| Band | Hex | App |
|---|---|---|
| Red | `#ff6464` | Fenrir: disk cleaner |
| Orange | `#ffa65c` | Ratatoskr: file copy |
| Gold | `#ffe066` | Mímir: content search |
| Green | `#5ce6a0` | Yggdrasil: file manager |
| Blue | `#5caeff` | Huginn: reserved |
| Violet | `#a582ff` | **Muninn**: photo search |
| All six | — | **Heimdall**: launcher |

An app uses **only its own band**, and only for: the caret, progress, focus rings, the focused field's underline, and the single primary action. Never for backgrounds, never as a gradient, and never for more than one element per view at rest.

## Neutrals ("night")

Cool blue-black, taken from the icon tile.

| Token | Hex | Use |
|---|---|---|
| `bg` | `#0a0b0f` | Window |
| `surface` | `#12131a` | Tiles, placeholders |
| `line` | `#1c1e26` | Hairlines, the only divider |
| `ghost` | `#363944` | Placeholder text |
| `low` | `#575a65` | Metadata |
| `mid` | `#8b8e99` | Secondary text, idle actions |
| `fg` | `#ecedf1` | Primary text |

## Type

**Satoshi** (400/500/700) everywhere, built into each binary.

- Wordmark: 700, lowercase, 15 px, `-0.01em`
- Hero field: 500, 64 px, `-0.04em`. The input is the headline.
- Eyebrow: 500, 11 px, uppercase, `0.16em` tracking, `low`
- Body: 400, 13–15 px; numbers use tabular figures

## Form

- **Two surfaces only.** Full windows (Muninn, Yggdrasil) are flat `bg`. Summoned overlays (Heimdall) are smoked glass over the desktop. Both use the same neutrals and the same hairline.
- Hairlines over boxes: there are no cards, pills or shadows inside a window.
- Corners: 2 px on content (photos), 4 px on keycaps, larger radii only on glass overlays.
- Motion: one curve, `cubic-bezier(.16, 1, .3, 1)`, at 150–600 ms. Animate type size and position, not colour.
- Progress is a 2 px band-coloured line across the top edge of the window.

## Icons

A 480 px squircle (`rx 112`) with a `#1c2030 → #08090e` tile and a 0.08 white hairline. Each icon pairs a polished metal object with arcs of the app's band seen through glass. Heimdall: a lens and a handle, all six arcs. Muninn: an iris, violet arcs, a pupil resting on the bridge.
