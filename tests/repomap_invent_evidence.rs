use std::{path::Path, process::Command};

fn compile_and_run(fixture: &str, label: &str) -> String {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let temporary = tempfile::tempdir().unwrap();
    let binary = temporary.path().join(label);
    let compile = Command::new("rustc")
        .current_dir(manifest)
        .args([
            "--crate-name",
            label,
            "--edition",
            "2021",
            "-Awarnings",
            fixture,
            "-O",
            "-o",
        ])
        .arg(&binary)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let first = Command::new(&binary).output().unwrap();
    let second = Command::new(&binary).output().unwrap();
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "{}",
        String::from_utf8_lossy(&second.stderr)
    );
    assert_eq!(
        first.stdout, second.stdout,
        "evaluator output changed across runs"
    );
    String::from_utf8(first.stdout).unwrap()
}

fn score(output: &str) -> f64 {
    let lines = output.lines().collect::<Vec<_>>();
    assert_eq!(
        lines.len(),
        1,
        "evaluator must print one score line: {output}"
    );
    let score: f64 = lines[0]
        .strip_prefix("SCORE: ")
        .expect("missing SCORE prefix")
        .parse()
        .expect("score was not numeric text");
    assert!(score.is_finite(), "score was not finite: {output}");
    score
}

#[test]
fn nexus_invent_frozen_and_heldout_repomap_evidence_stays_green() {
    let fitness = compile_and_run(
        "tests/fixtures/repomap_ranker_fitness.rs",
        "nexus_repomap_fitness",
    );
    let heldout = compile_and_run(
        "tests/fixtures/repomap_ranker_heldout.rs",
        "nexus_repomap_heldout",
    );
    assert!(score(&fitness) >= 79.0, "fitness regressed: {fitness}");
    assert!(score(&heldout) >= 74.0, "held-out regressed: {heldout}");
}
