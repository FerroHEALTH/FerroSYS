// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! The seeded sequence a variable draws from: `SplitMix64`, written here so
//! a seed names the same values in every version of the tool and on every
//! host, whatever a random-number crate later changes. No specification
//! governs this: our own design.

/// The `SplitMix64` increment, the golden ratio in 64 bits.
const GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// The FNV-1a 64-bit offset basis and prime, which fold a variable's name
/// into its seed.
const FNV_OFFSET: u64 = 0xCBF2_9CE4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01B3;

/// One variable's sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Rng {
    state: u64,
}

impl Rng {
    /// The sequence of the variable `name` under `seed`: two variables of one
    /// plan draw apart, and renaming one changes only its own sequence.
    pub(super) fn new(seed: u64, name: &str) -> Self {
        let folded = name.bytes().fold(FNV_OFFSET, |hash, byte| {
            (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
        });
        Self {
            state: seed ^ folded,
        }
    }

    /// The next 64 bits.
    pub(super) fn next_u64(&mut self) -> u64 {
        // NOTE: Steele, Lea and Flood, "Fast splittable pseudorandom number generators", OOPSLA 2014: the SplitMix64 mix.
        self.state = self.state.wrapping_add(GAMMA);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A value drawn evenly from `0..bound`; `None` for a zero bound.
    pub(super) fn below(&mut self, bound: u64) -> Option<u64> {
        // The draws under `threshold` would favour the low values, so they are
        // drawn again; at most half of all draws fall there.
        let threshold = bound.wrapping_neg().checked_rem(bound)?;
        loop {
            let draw = self.next_u64();
            if draw >= threshold {
                return draw.checked_rem(bound);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sequence_is_splitmix64() {
        // The first outputs of SplitMix64 from state 0, as its reference
        // implementation prints them.
        let mut rng = Rng { state: 0 };
        assert_eq!(0xE220_A839_7B1D_CDAF, rng.next_u64());
        assert_eq!(0x6E78_9E6A_A1B9_65F4, rng.next_u64());
        assert_eq!(0x06C4_5D18_8009_454F, rng.next_u64());
    }

    #[test]
    fn one_seed_and_name_give_one_sequence_and_another_name_another() {
        let draws = |seed, name| {
            let mut rng = Rng::new(seed, name);
            (0..4).map(|_| rng.next_u64()).collect::<Vec<_>>()
        };
        assert_eq!(draws(7, "patient"), draws(7, "patient"));
        assert_ne!(draws(7, "patient"), draws(7, "order"));
        assert_ne!(draws(7, "patient"), draws(8, "patient"));
    }

    #[test]
    fn a_bounded_draw_stays_below_its_bound_and_reaches_every_value() {
        let mut rng = Rng::new(1, "x");
        let mut seen = [0_u32; 7];
        for _ in 0..7_000 {
            let value = rng.below(7).unwrap();
            seen[usize::try_from(value).unwrap()] += 1;
        }
        assert!(seen.iter().all(|count| *count > 800), "{seen:?}");
        assert_eq!(None, rng.below(0));
        assert_eq!(Some(0), rng.below(1));
    }
}
