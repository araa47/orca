//! Short 3–4 letter name generator.

use std::collections::HashSet;

use rand::prelude::IndexedRandom;

const NAMES: &[&str] = &[
    "ace", "ash", "bay", "bex", "cal", "cob", "dax", "dex", "elm", "fen", "fig", "gus", "hap",
    "hex", "ivy", "jax", "jet", "kai", "kit", "lux", "max", "neo", "nix", "oak", "orb", "pax",
    "pip", "rex", "rio", "roo", "sal", "sky", "sol", "taj", "tex", "uri", "val", "vim", "wex",
    "yew", "zap", "zen", "zip", "blu", "cog", "dot", "ebb", "fin", "gem", "hue", "ink", "jot",
    "kip", "lox", "mud", "nub", "oat", "peg", "rig", "sap", "tab", "urn", "vex", "wok", "yam",
    "zag",
];

/// Return a short name not in `existing`.
///
/// Falls back to `w1000`–`w9999`, then `w10000`–`w99999`. Returns an error
/// if every candidate is already taken (extremely unlikely).
pub fn generate_name(existing: &HashSet<String>) -> Result<String, String> {
    let pool: Vec<&str> = NAMES
        .iter()
        .copied()
        .filter(|n| !existing.contains(*n))
        .collect();
    if let Some(name) = pool.choose(&mut rand::rng()) {
        return Ok((*name).to_string());
    }

    for range in [1000..=9999u32, 10000..=99999u32] {
        let candidates: Vec<u32> = range
            .filter(|n| !existing.contains(&format!("w{n}")))
            .collect();
        if let Some(&n) = candidates.choose(&mut rand::rng()) {
            return Ok(format!("w{n}"));
        }
    }

    Err("All worker names exhausted".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_name_from_pool() {
        let existing = HashSet::new();
        let name = generate_name(&existing).unwrap();
        assert!(
            NAMES.contains(&name.as_str()),
            "expected a name from NAMES, got {name}"
        );
    }

    #[test]
    fn avoids_existing_names() {
        let existing: HashSet<String> = NAMES.iter().map(|s| s.to_string()).collect();
        let name = generate_name(&existing).unwrap();
        assert!(name.starts_with('w'), "expected fallback name, got {name}");
        let num: u32 = name[1..].parse().expect("fallback should be w + digits");
        assert!((1000..=9999).contains(&num));
    }

    #[test]
    fn falls_back_to_second_range() {
        let mut existing: HashSet<String> = NAMES.iter().map(|s| s.to_string()).collect();
        for n in 1000..=9999u32 {
            existing.insert(format!("w{n}"));
        }
        let name = generate_name(&existing).unwrap();
        assert!(name.starts_with('w'), "expected fallback name, got {name}");
        let num: u32 = name[1..].parse().expect("fallback should be w + digits");
        assert!((10000..=99999).contains(&num));
    }

    #[test]
    fn returns_error_when_exhausted() {
        let mut existing: HashSet<String> = NAMES.iter().map(|s| s.to_string()).collect();
        for n in 1000..=99999u32 {
            existing.insert(format!("w{n}"));
        }
        let result = generate_name(&existing);
        assert!(result.is_err(), "expected error when all names exhausted");
        assert!(result.unwrap_err().contains("exhausted"));
    }

    #[test]
    fn name_count() {
        assert_eq!(NAMES.len(), 66);
    }
}
