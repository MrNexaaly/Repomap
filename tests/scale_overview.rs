use repomap::overview;
use std::fs;

#[test]
fn wide_root_subdivides_dominant_area_despite_nested_manifests() {
    let root = tempfile::tempdir().unwrap();
    for i in 0..20 {
        let dir = root.path().join(format!("area{i}"));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("one.rs"), "pub fn small() {}\n").unwrap();
    }
    for child in ["network", "graphics", "storage"] {
        let dir = root.path().join("devices").join(child);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("README.md"),
            format!("# {child}\n\nImplements {child} device support.\n"),
        )
        .unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = 'demo'\nversion = '0.1.0'\n",
        )
        .unwrap();
        for i in 0..1001 {
            fs::write(dir.join(format!("file{i}.rs")), "pub fn device() {}\n").unwrap();
        }
    }
    let map = overview(root.path(), 10_000, 40_000);
    assert!(map.ok);
    for child in ["network", "graphics", "storage"] {
        assert!(
            map.output.contains(&format!("devices/{child}/ | 1001 |")),
            "{}",
            map.output
        );
        assert!(map
            .output
            .contains(&format!("Implements {child} device support.")));
    }
    let zoom = overview(&root.path().join("devices/network"), 2000, 12000);
    assert!(zoom.ok);
    assert!(zoom.output.contains("Implements network device support."));
    assert!(!zoom.output.contains("devices/graphics"));
}
