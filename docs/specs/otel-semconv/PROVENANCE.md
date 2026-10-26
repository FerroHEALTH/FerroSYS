<!-- SPDX-FileCopyrightText: Cadasto B.V. -->
<!-- SPDX-License-Identifier: BUSL-1.1 -->

# Provenance: the OpenTelemetry semantic conventions

- Source: <https://github.com/open-telemetry/semantic-conventions>, release v1.44.0
- Commit: e10a930844c6951757a43b849d364f7d056ac32b
- Fetched: 2026-10-26 by `scripts/vendor/otel-semconv.sh`
- Licence: Apache License 2.0, upstream's `LICENSE`, copied beside the
  pages; the pages keep their upstream terms
- Contents: 11 files at their upstream paths, copied verbatim

- `LICENSE`: `c71d239df91726fc519c6eb72d318ec65820627232b2f796219e87dcf35d0ab4`
- `docs/http/http-metrics.md`: `165b5877ff13adb6b43d9c193c62c523d2326cd23e5f20fe5654993d15576b9a`
- `docs/http/http-spans.md`: `b789f491e75eb2f3d628cc6eb308865c2025fc9878d6faffaa60f8297f623150`
- `docs/resource/service.md`: `49f7beae0c85dd6a752d6d92e3d9e06f1495ece75389afd73c793ce4b51982b3`
- `docs/resource/deployment-environment.md`: `2ad813e2eabb9ded5e81a89fb61049a0973c818b0c8effb6b79b4ef5f65231e4`
- `docs/registry/attributes/deployment.md`: `a380631a3db01c0660491119736b39b925342b45217459b9c314bf1c517c6315`
- `docs/registry/attributes/error.md`: `3faf3b76c0ad7219e421128b5b0b53c77eb8b43f28ebe15407a3972bae004d96`
- `docs/registry/attributes/http.md`: `489e265386ca94254d8727f6c05ad3a7b1fcdeaf1ca7c63a06c1cc0d3528e2f1`
- `docs/registry/attributes/network.md`: `733548ab1ffdc25f2f24385af507b993904aca3fa25611f878b5eeaa27e42f7c`
- `docs/registry/attributes/service.md`: `08ae2e4717b9634e925c6df9f8e44ab6c670fc76dfd103855a4166e3d22b98c8`
- `docs/registry/attributes/url.md`: `0118940f88225417085812ae311fad3bb5fbf1c3911a606d6c2738c19f7537a6`

ferrohealth-telemetry cites these pages for the resource (`service.*`,
`deployment.environment.name`), the request span and the
`http.server.request.duration` histogram. Never hand-edit these files;
change the pin in `docs/VERSIONS.md` and re-run the script.
