<!-- SPDX-FileCopyrightText: Ruben Talstra -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# FerroSYS brand

FerroSYS follows the FerroHEALTH family system and takes its own mark and its
own hue, as every product in the family does. The file set, the naming and the
variants are shared with the family; the mark and the palette are FerroSYS's
own. Everything here is under the Business Source License 1.1 with the rest of
the repository.

## The mark

A gauge on a stand, for the control plane every server reports to. The dial is the full hue; the needle and the stand take the light value, so the reading is the first thing seen.

## Palette, "Olive & Iron"

| Token | Hex | Use |
|---|---|---|
| olive | `#5B6B16` | primary mark and accents; text on light |
| olive-light | `#BEF264` | the same voice on a dark ground; highlights in the mark |
| ink | `#0F172A` | text on light |
| mist | `#F1F5F9` | text on dark |
| tile | `#0B1020` | dark tile background |
| surface | `#F8FAFC` | light surface background |

The hue was chosen by measurement against the hues the family already owns:
worst-case CIEDE2000 distance over both grounds and normal, protanope and
deuteranope vision. The numbers and the bar are recorded in the family
repository's `assets/brand/README.md`, and the family site copies the two hue
values from `tokens.css` here verbatim.

## Files

| File | What it is |
|---|---|
| `ferrosys-icon.svg` | primary icon, full colour, transparent background, 64-unit viewBox at a 512 intrinsic size |
| `tokens.css` | the palette as CSS custom properties |

The lockups, the favicon set and the social card follow when the product has a
site to carry them.
