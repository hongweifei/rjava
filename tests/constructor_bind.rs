//! The "constructor binding" pattern: Java-side `new X()` directly yields a
//! Rust-backed object because the constructor's **body** calls a native
//! `init()` that binds Rust state to `this` — no factory, no post-bind.
//!
//! The constructor itself cannot be `native` (the JLS forbids it, `javac`
//! rejects it, and class-file rewriting is out of scope), so the binding
//! happens from the constructor's body, on the object under construction —
//! exactly what `rjava::userdata::bind` supports.
//!
//! Each test binary is its own process with its own JVM, so the native
//! registrations here cannot collide with any other binary's trampolines.
//!
//! If no JVM (or no `javac`) can be located at test time, every test skips
//! gracefully with an `eprintln` reason (the `jni` crate's java-locator uses
//! `JAVA_HOME`, then `java` on `PATH`, then the Windows registry).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};

use parking_lot::Mutex;
use rjava::prelude::*;
use rjava::{native_inst, JavaError};

/// The Rust state behind a `DirectCounter` shell, mirrored from the userdata
/// tests (`parking_lot::Mutex`).
struct DirectCounter(Mutex<i64>);

/// `DirectCounter.init()` — binds fresh state to `this` while the Java
/// constructor runs.
fn counter_init(env: &mut jni::Env, (this,): (JObject,)) -> JavaResult<()> {
    rjava::userdata::bind(env, &this, DirectCounter(Mutex::new(0)))
}

/// `DirectCounter.add(int)` — fetches the state bound by the constructor and
/// increments it, returning the new value.
fn counter_add(env: &mut jni::Env, (this, by): (JObject, i32)) -> JavaResult<i64> {
    let counter = rjava::userdata::get::<DirectCounter>(env, &this)?;
    let mut count = counter.0.lock();
    *count += by as i64;
    Ok(*count)
}

/// Compile the `constructor_bind` fixture exactly once per test process.
/// `Err` carries the reason, reported once; when it fails every test skips.
static FIXTURE_COMPILED: LazyLock<Result<(), String>> = LazyLock::new(compile_fixture);

fn compile_fixture() -> Result<(), String> {
    // `javac -d` creates the output directory itself; create it first anyway
    // so a failure here is reported as a compile problem, not a mystery.
    let out_dir = "target/rjava-constructor-classes";
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
        .arg("tests/java/constructor_bind/DirectCounter.java")
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
        .class_path("target/rjava-constructor-classes")
        .build()
    {
        Ok(java) => Some(java),
        Err(e) => {
            eprintln!("SKIPPING test: no JVM available: {e}");
            None
        }
    }
}

/// Register `init` and `add` on `DirectCounter` exactly once per test
/// process. Type-derived `native_inst!` registrations: `(JObject,)->()` and
/// `(JObject, i32)->i64` are unique in this binary, so no shared-trampoline
/// collision is possible.
static NATIVES_REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();

/// True once [`register_natives`] has succeeded. Lets the pre-registration
/// test observe the failure state only when it genuinely holds it (see
/// `constructor_binding_requires_registration`).
static REGISTERED: AtomicBool = AtomicBool::new(false);

fn register_natives() -> Result<(), String> {
    let result = NATIVES_REGISTERED
        .get_or_init(|| {
            let java = match jvm() {
                Some(java) => java,
                None => return Err("no JVM available".to_string()),
            };
            let result: JavaResult<()> = (|| {
                let clazz = java.class("constructor_bind.DirectCounter")?;
                clazz.register_natives(&[
                    native_inst!("init", counter_init)?,
                    native_inst!("add", counter_add)?,
                ])
            })();
            result.map_err(|e| e.to_string())
        })
        .clone();
    if result.is_ok() {
        REGISTERED.store(true, Ordering::SeqCst);
    }
    result
}

/// Serializes the constructor-binding tests: they share one JVM and one
/// registration, and Rust runs tests in a process on parallel threads —
/// without the lock the pre-registration test could race the OnceLock
/// registration of the other tests.
static CTOR_BIND_LOCK: Mutex<()> = Mutex::new(());

/// `new DirectCounter()` runs the plain-Java constructor, whose body calls
/// the native `init()` — so the returned object is already bound, no factory
/// and no post-construction `bind` involved.
#[test]
fn new_constructs_bound_object() {
    let _guard = CTOR_BIND_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING constructor_bind test: {reason}");
        return;
    }
    let counter = java
        .new_object("constructor_bind.DirectCounter", ())
        .expect("new DirectCounter() must bind state in the constructor");
    // The state lives in Rust and persists across calls — bound by the ctor.
    let v: i64 = counter.call("add", (5_i32,)).unwrap();
    assert_eq!(v, 5);
    let v: i64 = counter.call("add", (3_i32,)).unwrap();
    assert_eq!(v, 8);
}

/// Two `new DirectCounter()` objects bind distinct state: no cross-talk.
#[test]
fn new_objects_are_independent() {
    let _guard = CTOR_BIND_LOCK.lock();
    let Some(java) = jvm() else { return };
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING constructor_bind test: {reason}");
        return;
    }
    let a = java
        .new_object("constructor_bind.DirectCounter", ())
        .unwrap();
    let b = java
        .new_object("constructor_bind.DirectCounter", ())
        .unwrap();
    let _: i64 = a.call("add", (10_i32,)).unwrap();
    let _: i64 = b.call("add", (99_i32,)).unwrap();
    let va: i64 = a.call("add", (0_i32,)).unwrap();
    let vb: i64 = b.call("add", (0_i32,)).unwrap();
    assert_eq!((va, vb), (10, 99));
}

/// The binder is a *registered native*: constructing before registration
/// must fail. This test holds [`CTOR_BIND_LOCK`], so if it wins the race for
/// the first construction it observes the unregistered state; if a parallel
/// test already registered (also under the lock), the negative half is
/// skipped — deterministic in either ordering.
#[test]
fn constructor_binding_requires_registration() {
    let _guard = CTOR_BIND_LOCK.lock();
    let Some(java) = jvm() else { return };

    if !REGISTERED.load(Ordering::SeqCst) {
        // `init()` has no registered implementation yet: the constructor's
        // call to it throws before `new` completes. The JVM raises
        // `UnsatisfiedLinkError` for a declared native method with no
        // registered implementation (`NoSuchMethodError` is the
        // `RegisterNatives`-side failure for a name/signature that matches
        // no declared method — a different, registration-time error).
        let err = java
            .new_object("constructor_bind.DirectCounter", ())
            .expect_err("constructing before registration must fail");
        match &err {
            JavaError::WithContext { operation, source } => {
                assert_eq!(
                    operation, "constructing constructor_bind.DirectCounter()",
                    "operation must name the failed construction"
                );
                match &**source {
                    JavaError::JavaException { class, message } => {
                        assert_eq!(
                            class, "java.lang.UnsatisfiedLinkError",
                            "unexpected exception class: {class}"
                        );
                        eprintln!(
                            "pre-registration construction failed as expected: {class}: {message}"
                        );
                    }
                    other => panic!("expected a JavaException source, got {other:?}"),
                }
            }
            other => panic!("expected WithContext, got {other:?}"),
        }
    }

    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING constructor_bind test: {reason}");
        return;
    }
    let counter = java
        .new_object("constructor_bind.DirectCounter", ())
        .expect("after registration, new DirectCounter() must bind in the constructor");
    let v: i64 = counter.call("add", (7_i32,)).unwrap();
    assert_eq!(v, 7);
}
