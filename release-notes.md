
### Changed

- Breaking: the plan's `[load] max_lag_ms` is optional
  (`LoadSettings::max_lag_ms` is an `Option<u64>`), and when a plan names
  none the generator's lag bound is half the interval between two
  arrivals, `500 / rate` ms (`LoadSettings::lag_bound_ms`): 10 ms at the
  default 50 per second as before, 25 ms at 20. Every latency is timed
  from the intended instant, so the lag hides nothing; the bound keeps
  each arrival between its neighbours and an A/B pair's arms taking
  turns. A plan that names `max_lag_ms` keeps it. On 4-vCPU hosted
  runners the fixed 10 ms refused two thirds of FerroFED's phases while
  their schedule held (#357).
- Breaking: an A/B pair is judged steady when the head's second half over
  its first, over the base's, lies within 0.8 to 1.25 on p50 and p95; a
  drift of the runner that reaches both arms no longer makes the run one
  that cannot be judged. `AbReport` gains `wanted`, the judgeable pairs
  the run set out to measure: a pair that cannot be judged is left out
  with a warning and one more pair, led by the same arm, is measured in
  its place, up to `ab::EXTRA_PAIRS` (two) beyond `--pairs`, so one such
  pair no longer costs the product a whole retry on a fresh runner. A
  report without `wanted` judges every pair, as before (#358).

### Support period

Only the newest release is supported: there are no maintenance branches, no long-term-support line and no backports, and upgrading is free. Under the Cyber Resilience Act, Cadasto B.V. commits to keep publishing security fixes for at least five years after each release (Art 13(8)) and ships them only in the newest release (Art 13(10)). For this release that commitment runs until the end of November 2031 (Regulation (EU) 2024/2847 Art 13(19)); see [SECURITY.md](https://github.com/FerroHEALTH/FerroSYS/blob/main/SECURITY.md#supported-versions).
