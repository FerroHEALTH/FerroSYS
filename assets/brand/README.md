<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# FerroSYS brand

FerroSYS follows the FerroHEALTH family system. It keeps its own mark and the
olive palette below, and, as a repository of shared crates like FerroAUTH and
FerroAUDIT, its lockup is drawn in the parent's steel. The file set, the
naming and the variants are shared with the family. Everything here is under the Business Source License 1.1 with the rest of
the repository.

## The mark

A gauge on a stand, for the running state of a server that these crates
log, measure and report. The dial is the full hue; the needle and the stand
take the light value, so the reading is the first thing seen.

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

## The lockup

The family's brand rule gives the shared libraries (FerroAUTH, FerroAUDIT,
FerroUI, FerroSYS) no hue of their own in a lockup and shows them in the
parent's steel. The lockup draws the dial and its hub in steel (`#3F6070`,
steel-light `#8FB6C4` on a dark ground) with the needle and the stand in
steel-light (mist `#F1F5F9` on a dark ground), and sets the wordmark as
FerroHEALTH's is: "Ferro" in iron (`#0F172A`, mist on a dark ground) and
"SYS" in steel (steel-light on a dark ground), in Bricolage Grotesque 700,
after FerroHEALTH's `assets/brand/ferrohealth-lockup-auto.svg`. The icon and
the olive palette above stay as they are.

## Files

| File | What it is |
|---|---|
| `ferrosys-icon.svg` | primary icon, full colour, transparent background, 64-unit viewBox at a 512 intrinsic size |
| `ferrosys-lockup-auto.svg` | the gauge beside the wordmark in steel for the README and any page that serves one file to both themes, switched on `prefers-color-scheme` |
| `tokens.css` | the palette as CSS custom properties |

The favicon set and the social card follow when the product has a site to
carry them.
