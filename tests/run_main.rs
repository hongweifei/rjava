//! `Java::run_main` — invoking a Java `public static void main(String[])`
//! entry point from Rust.
//!
//! Each test binary is its own process with its own JVM, so the
//! system-property probe in the `MainProbe` fixture is per-process and safe.
//!
//! If no JVM (or no `javac`) can be located at test time, every test skips
//! gracefully with an `eprintln` reason (the `jni` crate's java-locator uses
//! `JAVA_HOME`, then `java` on `PATH`, then the Windows registry).

use std::sync::{LazyLock, Mutex};

use rjava::prelude::*;

/// Serializes the two `MainProbe` tests: both set the same system property
/// (`rjava.main.args`), and Rust runs tests in a process on parallel threads
/// against one shared JVM — without the lock one test's `main` can overwrite
/// the other's property before it is read back.
static PROBE_LOCK: Mutex<()> = Mutex::new(());

/// Compile the `runmain` fixtures exactly once per test process. `Err`
/// carries the reason, reported once; when it fails every test skips.
static FIXTURE_COMPILED: LazyLock<Result<(), String>> = LazyLock::new(compile_fixture);

fn compile_fixture() -> Result<(), String> {
    // `javac -d` creates the output directory itself; create it first anyway
    // so a failure here is reported as a compile problem, not a mystery.
    let out_dir = "target/rjava-runmain-classes";
    std::fs::create_dir_all(out_dir).map_err(|e| format!("cannot create {out_dir}: {e}"))?;
    let javac = match std::env::var("JAVA_HOME") {
        Ok(home) => format!(
            "{home}/bin/javac{}",
            if cfg!(windows) { ".exe" } else { "" }
        ),
        Err(_) => "javac".to_string(),
    };
    let output = std::process::Command::new(&javac)
        .arg("-d")
        .arg(out_dir)
        .arg("tests/java/runmain/MainProbe.java")
        .arg("tests/java/runmain/MainThrows.java")
        .output()
        .map_err(|e| format!("could not run `{javac}`: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{javac}` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

/// Attempt to build the JVM; returns `None` (after an `eprintln`) if no JVM
/// is available so tests can skip instead of failing on JVM-less machines.
fn jvm() -> Option<Java> {
    if let Err(reason) = &*FIXTURE_COMPILED {
        eprintln!("SKIPPING test: could not compile Java fixture: {reason}");
        return None;
    }
    match Java::builder()
        .class_path("target/rjava-runmain-classes")
        .build()
    {
        Ok(java) => Some(java),
        Err(e) => {
            eprintln!("SKIPPING test: no JVM available: {e}");
            None
        }
    }
}

#[test]
fn run_main_invokes_entry_point() {
    let Some(java) = jvm() else { return };
    let _guard = PROBE_LOCK.lock().expect("probe lock");
    java.run_main("runmain.MainProbe", &["a", "b", "c"])
        .expect("main should run");
    let sys = java.class("java.lang.System").expect("System class");
    let args: Option<String> = sys
        .call_static("getProperty", ("rjava.main.args",))
        .expect("getProperty should work");
    assert_eq!(args.as_deref(), Some("a,b,c"));
}

#[test]
fn run_main_zero_args() {
    let Some(java) = jvm() else { return };
    let _guard = PROBE_LOCK.lock().expect("probe lock");
    java.run_main("runmain.MainProbe", &[] as &[&str])
        .expect("main should run with no args");
    let sys = java.class("java.lang.System").expect("System class");
    let args: Option<String> = sys
        .call_static("getProperty", ("rjava.main.args",))
        .expect("getProperty should work");
    assert_eq!(args.as_deref(), Some(""));
}

#[test]
fn run_main_exception_surfaces() {
    let Some(java) = jvm() else { return };
    let err = java.run_main("runmain.MainThrows", &[] as &[&str]).unwrap_err();
    match err {
        JavaError::WithContext { operation, source } => {
            assert!(
                operation.starts_with("calling main(") && operation.contains("on runmain.MainThrows"),
                "operation must name the failed main call: {operation}"
            );
            match *source {
                JavaError::JavaException { class, message } => {
                    assert!(
                        class.contains("IllegalArgumentException"),
                        "unexpected exception class: {class}"
                    );
                    assert!(
                        message.contains("boom from main"),
                        "unexpected message: {message}"
                    );
                }
                other => panic!("expected a JavaException source, got {other:?}"),
            }
        }
        other => panic!("expected WithContext, got {other:?}"),
    }
}

#[test]
fn run_main_missing_class_errors() {
    let Some(java) = jvm() else { return };
    let err = java.run_main("no.such.Main", &[] as &[&str]).unwrap_err();
    // `find_class` fails with a raw JNI `NoClassDefFound` error rather than a
    // captured Java exception — any `Err` is the contract here.
    assert!(matches!(err, JavaError::Jni(_)), "unexpected error: {err:?}");
}
