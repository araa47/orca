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
