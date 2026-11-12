
### Changed

- Breaking: `HistogramError::Base64` carries `histogram::NotBase64`, the
  crate's own type, in place of `base64::DecodeError`. Its `Display` is the
  decoder's account of the offending symbol or length, as before, and the
  crate's API names no `base64` type, so a later move of that dependency's
  line is no break. The crate takes base64 0.23.1 (#383).


### Support period

Only the newest release is supported: there are no maintenance branches, no long-term-support line and no backports, and upgrading is free. Under the Cyber Resilience Act, Cadasto B.V. commits to keep publishing security fixes for at least five years after each release (Art 13(8)) and ships them only in the newest release (Art 13(10)). For this release that commitment runs until the end of November 2031 (Regulation (EU) 2024/2847 Art 13(19)); see [SECURITY.md](https://github.com/FerroHEALTH/FerroSYS/blob/main/SECURITY.md#supported-versions).
