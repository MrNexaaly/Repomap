use std::{fs, process::Command};

fn run(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_repomap"))
        .args(arguments)
        .output()
        .expect("native repomap CLI starts")
}

#[test]
fn bare_invocation_is_the_overview() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("README.md"),
        "# Demo\n\nA demo service that greets people.\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("main.rs"),
        "//! Greets people.\nfn main() {}\n",
    )
    .unwrap();
    let output = run(&[directory.path().to_str().unwrap()]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("repository overview"), "{stdout}");
    assert!(
        stdout.contains("A demo service that greets people."),
        "{stdout}"
    );
    assert!(stdout.contains("## Files"), "{stdout}");
}

#[test]
fn legacy_terminal_interface_uses_the_native_structural_map() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("main.rs"),
        "mod helper;\nfn main() {}\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("helper.rs"),
        "pub fn helper() -> u8 { 1 }\n",
    )
    .unwrap();

    let output = run(&[
        directory.path().to_str().unwrap(),
        "--list",
        "--max-chars",
        "12000",
    ]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.starts_with("main.rs |"), "{stdout}");
    assert!(stdout.contains("helper.rs"), "{stdout}");
    assert!(stdout.contains("fn helper"), "{stdout}");
}

#[test]
fn query_cli_selects_the_definition_and_caller_under_a_tight_budget() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("payments.rs"),
        "pub fn authorize_payment() {}\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("checkout.rs"),
        "pub fn submit_order() { authorize_payment(); }\n",
    )
    .unwrap();
    fs::write(
        directory.path().join("unrelated.rs"),
        "pub fn unrelated() {}\n".repeat(300),
    )
    .unwrap();

    let output = run(&[
        directory.path().to_str().unwrap(),
        "--query",
        "authorize payment submit order",
        "--mentioned-symbol",
        "authorize_payment",
        "--token-budget",
        "35",
    ]);
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("payments.rs"), "{stdout}");
    assert!(stdout.contains("checkout.rs"), "{stdout}");
    assert!(!stdout.contains("unrelated.rs"), "{stdout}");
}

#[test]
fn help_and_invalid_arguments_are_explicit() {
    let help = run(&["--help"]);
    let help_text = String::from_utf8(help.stdout).unwrap();
    assert!(help.status.success());
    assert!(help_text.contains("Native query-aware repository map"));
    assert!(help_text.contains("--mentioned-symbol"));

    let invalid = run(&["--max-chars", "0"]);
    assert_eq!(invalid.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&invalid.stderr).contains("must be at least 1"));
}

#[test]
fn maintainers_names_areas_and_zoomed_maps_find_it() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(
        root.join("MAINTAINERS"),
        "THE REST\nF:\t*\nF:\t*/\n\nEXT4 FILE SYSTEM\nL:\tlinux-ext4@example.org\nF:\tfs/ext4/\n\nNETWORKING DRIVERS\nF:\tdrivers/net/\n\nINTEL ETHERNET DRIVERS\nF:\tdrivers/net/ethernet/intel/\n",
    )
    .unwrap();
    for file in [
        "fs/ext4/inode.c",
        "fs/ext4/super.c",
        "drivers/net/tun.c",
        "drivers/net/ethernet/intel/igb.c",
        "init/main.c",
    ] {
        let path = root.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "int x;\n").unwrap();
    }
    let top = String::from_utf8(run(&[root.to_str().unwrap()]).stdout).unwrap();
    assert!(top.contains("EXT4 FILE SYSTEM"), "{top}");
    assert!(top.contains("NETWORKING DRIVERS"), "{top}");
    assert!(
        !top.contains("THE REST"),
        "catch-all sections name nothing: {top}"
    );
    let zoomed =
        String::from_utf8(run(&[root.join("drivers/net").to_str().unwrap()]).stdout).unwrap();
    assert!(zoomed.contains("INTEL ETHERNET DRIVERS"), "{zoomed}");
    assert!(
        zoomed.contains("drivers/net/<directory>") || zoomed.contains("/<directory>"),
        "{zoomed}"
    );
}
