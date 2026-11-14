//! The third-party software the app is built from, as Settings → Info lists it. The list is
//! `assets/third-party.tsv` (name, version, license per line), written by `bun run third-party`
//! from Cargo's dependency graph; `bun run lint` fails while it is out of date.

use std::sync::LazyLock;

/// One library or bundled file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Component {
    pub name: &'static str,
    pub version: &'static str,
    /// An SPDX license expression, such as `MIT OR Apache-2.0`.
    pub license: &'static str,
}

const LIST: &str = include_str!("../assets/third-party.tsv");

static ALL: LazyLock<Vec<Component>> = LazyLock::new(|| LIST.lines().filter_map(parse).collect());

/// Every component, sorted by name.
pub fn all() -> &'static [Component] {
    &ALL
}

fn parse(line: &'static str) -> Option<Component> {
    let mut fields = line.split('\t');
    let component = Component { name: fields.next()?, version: fields.next()?, license: fields.next()? };
    (fields.next().is_none() && !component.name.is_empty()).then_some(component)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_should_name_a_component_with_its_version_and_license() {
        assert_eq!(all().len(), LIST.lines().count());
        assert!(all().iter().all(|c| !c.version.is_empty() && !c.license.is_empty()));
    }

    #[test]
    fn the_list_should_include_the_matrix_sdk_and_the_bundled_font() {
        assert!(all().iter().any(|c| c.name == "matrix-sdk") && all().iter().any(|c| c.name == "Inter (font)"));
    }
}
