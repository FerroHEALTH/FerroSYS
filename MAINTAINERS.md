<!-- SPDX-FileCopyrightText: Ruben Talstra -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Maintainers and access continuity

This file is the roster and the honest answer to the question an enterprise
procurement review asks about any infrastructure software: what happens if the
people who can ship a fix are unavailable? It describes the project as it is on
the day you read it in git history.

## Roster

| Person        | GitHub                                           | Role              | Since      |
|---------------|--------------------------------------------------|-------------------|------------|
| Ruben Talstra | [@rubentalstra](https://github.com/rubentalstra) | Maintainer (sole) | 2026-09-14 |

**The bus factor of this project is one.** There is exactly one person with
write access to the repository, one person who can publish a release, and one
person who can accept a pull request. No organisation stands behind the project
and no legal entity is a party to it.

## If the maintainer is unavailable

- **Nothing already published disappears.** A published release is immutable
  and its tag cannot be moved or deleted.
- **Nothing new ships.** No release and no security fix.
- **The work is not lost.** The licence is the Business Source License 1.1,
  which converts to Apache 2.0 four years after each version; the history is
  public and every design decision is in the tree or on the tracker. A fork is
  a complete and legitimate continuation, and the project's position is that it
  should be taken rather than waited on.

If you depend on this software and that position is not acceptable to you, the
mitigation is on your side of the boundary: pin a version, keep a fork you can
build, and budget for maintaining it.

## Commercial licensing

The maintainer above is the Licensor named in [LICENSE](LICENSE) and the
contact for a commercial licence.
