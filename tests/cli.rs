//! End-to-end checks of the binary as the grader invokes it: `payments-engine FILE_PATH`.
//!
//! The important property is that stdout carries only the account CSV, whatever happens on
//! stderr, and that usage errors fail with a non-zero exit code and an empty stdout.

use std::fs;
use std::path;
use std::process;

fn run_binary(args: &[&str]) -> process::Output {
    process::Command::new(env!("CARGO_BIN_EXE_payments-engine"))
        .args(args)
        .env_remove("RUST_LOG")
        .output()
        .expect("binary starts")
}

#[test]
fn task_cases_csv_prints_the_verified_account_report() {
    let input = path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/test_data.csv");

    let output = run_binary(&[input.to_str().expect("UTF-8 path")]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        include_bytes!("../examples/test_data.expected.csv")
    );
}

#[test]
fn spec_example_with_no_file_extension_prints_the_required_report() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let input = dir.path().join("transactions");
    fs::write(
        &input,
        b"type, client, tx, amount\ndeposit, 1, 1, 1.0\ndeposit, 2, 2, 2.0\ndeposit, 1, 3, 2.0\nwithdrawal, 1, 4, 1.5\nwithdrawal, 2, 5, 3.0\n",
    )
    .expect("write input");

    let output = run_binary(&[input.to_str().expect("UTF-8 path")]);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout,
        b"client,available,held,total,locked\n1,1.5,0,1.5,false\n2,2,0,2,false\n"
    );
}

#[test]
fn rust_log_selects_the_log_level() {
    let input = path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/test_data.csv");
    let run_with = |level: &str| {
        let output = process::Command::new(env!("CARGO_BIN_EXE_payments-engine"))
            .arg(&input)
            .env("RUST_LOG", level)
            .output()
            .expect("binary starts");
        String::from_utf8_lossy(&output.stderr).into_owned()
    };

    let debug = run_with("debug");
    let filtered = run_with("warn,payments_engine::account_writer=debug");
    let off = run_with("off");

    assert!(debug.contains("transaction rejected"), "{debug}");
    assert!(filtered.contains("transaction rejected"), "{filtered}");
    assert!(!filtered.contains("CSV reader started"), "{filtered}");
    assert!(off.is_empty(), "{off}");
}

#[test]
fn trace_logs_cover_worker_threads_and_preserve_csv_stdout() {
    let input = path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/test_data.csv");

    let output = process::Command::new(env!("CARGO_BIN_EXE_payments-engine"))
        .arg(input)
        .env("RUST_LOG", "trace")
        .output()
        .expect("binary starts");

    assert!(output.status.success());
    assert_eq!(
        output.stdout,
        include_bytes!("../examples/test_data.expected.csv")
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    for expected in ["transaction parsed", "deposit reversed and account locked"] {
        assert!(stderr.contains(expected), "missing {expected:?}: {stderr}");
    }
}

#[test]
fn malformed_row_logs_its_location_and_error() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let input = dir.path().join("transactions.csv");
    fs::write(
        &input,
        b"type,client,tx,amount\ndeposit,1,1,1\ndeposit,1,2,\n",
    )
    .expect("write input");

    let output = run_binary(&[input.to_str().expect("UTF-8 path")]);

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("invalid transaction row"), "{stderr}");
    assert!(stderr.contains("line=Some(3)"), "{stderr}");
    assert!(stderr.contains("processing failed"), "{stderr}");
}

#[test]
fn malformed_fields_are_not_disclosed_in_diagnostics() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let input = dir.path().join("transactions.csv");
    for (row, column) in [
        ("private_value,1,2,1", None),
        ("deposit,private_value,2,1", Some(1)),
        ("deposit,1,private_value,1", Some(2)),
    ] {
        fs::write(&input, format!("type,client,tx,amount\n{row}\n")).expect("write input");
        let output = process::Command::new(env!("CARGO_BIN_EXE_payments-engine"))
            .arg(&input)
            .env("RUST_LOG", "trace")
            .output()
            .expect("binary starts");

        assert!(!output.status.success());
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("invalid transaction row"), "{stderr}");
        assert!(stderr.contains("line=Some(2)"), "{stderr}");
        assert!(!stderr.contains("private_value"), "{stderr}");
        if let Some(column) = column {
            assert!(
                stderr.contains(&format!("zero-based column {column}")),
                "{stderr}"
            );
        }
    }
}

#[test]
fn wrong_argument_count_fails_with_usage_and_no_stdout() {
    let output = run_binary(&[]);

    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(String::from_utf8_lossy(&output.stderr).contains("Usage:"));
}

#[test]
fn missing_file_fails_with_no_stdout() {
    let output = run_binary(&["does-not-exist.csv"]);

    assert!(!output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "");
    assert!(String::from_utf8_lossy(&output.stderr).contains("does-not-exist.csv"));
}

#[test]
fn unreadable_input_returns_an_error() {
    // Opening a directory succeeds on Linux; the first read fails.
    let output = run_binary(&[env!("CARGO_MANIFEST_DIR")]);

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Failed to read"));
}
