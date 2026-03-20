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
mod tests;
