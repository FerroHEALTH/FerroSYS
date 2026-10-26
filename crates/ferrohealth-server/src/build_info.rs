// SPDX-FileCopyrightText: Cadasto B.V.
// SPDX-License-Identifier: BUSL-1.1

//! What a build is, and the boot banner that says so.
//!
//! A product's build script captures the commit, the build instant and the
//! compiler as environment variables, and the binary fills [`BuildInfo`]
//! with `env!` and `option_env!`. The banner is the text a binary prints to
//! stdout before the log subscriber starts, and only when the console
//! renders `pretty`, so a log pipeline reading `json` never sees it. Colour
//! is left to the product: the banner reads the same in a pipe.

use ferrohealth_telemetry::section::Rendering;

/// The facts of one build.
///
/// # Examples
///
/// ```
/// use ferrohealth_server::build_info::BuildInfo;
///
/// let info = BuildInfo::new("ferrosys", "0.2.0").with_commit("1a2b3c4d5e6f");
/// assert_eq!(Some("1a2b3c4"), info.short_commit());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildInfo {
    name: &'static str,
    version: &'static str,
    commit: Option<&'static str>,
    built_at: Option<&'static str>,
    rustc: Option<&'static str>,
}

impl BuildInfo {
    /// The build of `name` at `version`.
    #[must_use]
    pub const fn new(name: &'static str, version: &'static str) -> Self {
        Self {
            name,
            version,
            commit: None,
            built_at: None,
            rustc: None,
        }
    }

    /// Adds the commit the build was made from.
    #[must_use]
    pub const fn with_commit(mut self, commit: &'static str) -> Self {
        self.commit = Some(commit);
        self
    }

    /// Adds the instant the build was made, RFC 3339.
    #[must_use]
    pub const fn with_built_at(mut self, built_at: &'static str) -> Self {
        self.built_at = Some(built_at);
        self
    }

    /// Adds the compiler that made the build.
    #[must_use]
    pub const fn with_rustc(mut self, rustc: &'static str) -> Self {
        self.rustc = Some(rustc);
        self
    }

    /// The product name.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        self.name
    }

    /// The version.
    #[must_use]
    pub const fn version(&self) -> &'static str {
        self.version
    }

    /// The commit, when the build recorded one.
    #[must_use]
    pub const fn commit(&self) -> Option<&'static str> {
        self.commit
    }

    /// The commit's first seven characters, when the build recorded one.
    #[must_use]
    pub fn short_commit(&self) -> Option<&'static str> {
        self.commit.map(|commit| commit.get(..7).unwrap_or(commit))
    }

    /// The build instant, when the build recorded one.
    #[must_use]
    pub const fn built_at(&self) -> Option<&'static str> {
        self.built_at
    }

    /// The compiler, when the build recorded one.
    #[must_use]
    pub const fn rustc(&self) -> Option<&'static str> {
        self.rustc
    }
}

/// The banner of `info`, or `None` unless the console renders `pretty`.
///
/// It holds the product's `wordmark` lines, the build line, then one line per
/// entry of `facts`: the pins it serves, the lanes its configuration switches
/// on.
#[must_use]
pub fn banner(
    info: &BuildInfo,
    wordmark: &str,
    facts: &[(&str, String)],
    rendering: Rendering,
) -> Option<String> {
    if rendering != Rendering::Pretty {
        return None;
    }
    let mut lines: Vec<String> = wordmark.lines().map(str::to_owned).collect();
    let parts = [
        Some(format!("{} {}", info.name, info.version)),
        info.short_commit().map(|commit| format!(" ({commit})")),
        info.built_at.map(|built_at| format!(", built {built_at}")),
        info.rustc.map(|rustc| format!(" with {rustc}")),
    ];
    lines.push(parts.into_iter().flatten().collect());
    let width = facts
        .iter()
        .map(|(name, _)| name.chars().count())
        .max()
        .unwrap_or(0);
    lines.extend(
        facts
            .iter()
            .map(|(name, value)| format!("  {name:<width$}  {value}")),
    );
    let mut text = lines.join("\n");
    text.push('\n');
    Some(text)
}

#[cfg(test)]
mod tests {
    use ferrohealth_telemetry::section::Rendering;

    use super::{BuildInfo, banner};

    const INFO: BuildInfo = BuildInfo::new("ferrosys", "0.2.0")
        .with_commit("1a2b3c4d5e6f")
        .with_built_at("2026-10-26T12:00:00Z")
        .with_rustc("rustc 1.99.1");

    #[test]
    fn the_banner_prints_only_for_pretty() {
        assert!(banner(&INFO, "FERROSYS", &[], Rendering::Json).is_none());
        let text = banner(
            &INFO,
            "FERROSYS",
            &[
                ("FHIR", "R4".to_owned()),
                ("lanes", "facade, hl7v2".to_owned()),
            ],
            Rendering::Pretty,
        )
        .expect("pretty prints the banner");
        assert_eq!(
            "FERROSYS\n\
             ferrosys 0.2.0 (1a2b3c4), built 2026-10-26T12:00:00Z with rustc 1.99.1\n\
             \x20 FHIR   R4\n\
             \x20 lanes  facade, hl7v2\n",
            text
        );
    }

    #[test]
    fn a_build_without_its_facts_still_names_itself() {
        let info = BuildInfo::new("ferrosys", "0.2.0");
        assert_eq!(None, info.short_commit());
        assert_eq!(
            Some("ferrosys 0.2.0\n".to_owned()),
            banner(&info, "", &[], Rendering::Pretty)
        );
        assert_eq!(("ferrosys", "0.2.0"), (info.name(), info.version()));
        assert_eq!(
            (None, None, None),
            (info.commit(), info.built_at(), info.rustc())
        );
        assert_eq!(
            Some("abc"),
            BuildInfo::new("x", "1").with_commit("abc").short_commit()
        );
    }
}
