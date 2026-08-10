//! # Rust-backed Java objects (userdata) — the mlua `UserData` analog
//!
//! A Java object whose **state lives in Rust**:
//!
//! 1. a plain Java class (`Counter`) declares a static factory
//!    `public static native Counter create();` — native *constructors* are
//!    illegal in Java source (JLS forbids `native` on constructors), hence
//!    the factory — and instance natives `increment` / `value`;
//! 2. the Rust `counter_create` implementation builds the shell with
//!    [`rjava::userdata::create_shell`], which constructs the Java object and
//!    binds a Rust value to it in a process-global registry;
//! 3. the instance natives look the state back up with
//!    [`rjava::userdata::get`] and call its methods.
//!
//! The registry is keyed by the Java object's **own identity**
//! (`System.identityHashCode(obj)` — the `xxxxxx` in `ClassName@xxxxxx`), so
//! the shell class needs **no handle field** — it is a plain Java class, and
//! Java code could equally `new` it and let the host bind state later.
//!
//! This example is self-contained: the `Counter.java` source is embedded
//! below, compiled with `javac` (via `JAVA_HOME/bin/javac`, like the
//! integration tests) into `target/userdata-classes`, and the JVM is started
//! with that directory on the class path.
//!
//! Run with: `cargo run --example userdata`

use rjava::prelude::*;
use rjava::{native, native_inst};

// ---------------------------------------------------------------------------
// The Java source, embedded so the example is one self-contained file
// ---------------------------------------------------------------------------

/// `com.example.Counter` — a plain Java class; the state lives in Rust.
const COUNTER_JAVA: &str = r#"
package com.example;

public class Counter {
    public static native Counter create();
    public native long increment(long by);
    public native long value();
}
"#;

// ---------------------------------------------------------------------------
// The Rust state and the native implementations
// ---------------------------------------------------------------------------

/// The Rust-side state behind a `Counter` shell. `Mutex` so concurrent
/// increments stay deterministic.
struct Counter {
    count: parking_lot::Mutex<i64>,
}

impl Counter {
    fn new() -> Self {
        Counter {
            count: parking_lot::Mutex::new(0),
        }
    }

    fn increment(&self, by: i64) -> i64 {
        let mut count = self.count.lock();
        *count += by;
        *count
    }

    fn value(&self) -> i64 {
        *self.count.lock()
    }
}

fn counter_create(env: &mut jni::Env, (): ()) -> JavaResult<JObject> {
    rjava::userdata::create_shell(env, "com.example.Counter", Counter::new())
}

fn counter_increment(env: &mut jni::Env, (this, by): (JObject, i64)) -> JavaResult<i64> {
    let counter = rjava::userdata::get::<Counter>(env, &this)?;
    Ok(counter.increment(by))
}

fn counter_value(env: &mut jni::Env, (this,): (JObject,)) -> JavaResult<i64> {
    let counter = rjava::userdata::get::<Counter>(env, &this)?;
    Ok(counter.value())
}

// ---------------------------------------------------------------------------
// javac helper (JAVA_HOME/bin/javac(.exe), like the integration tests)
// ---------------------------------------------------------------------------

/// `javac` from `JAVA_HOME/bin` (falling back to `PATH`).
fn tool(name: &str) -> String {
    match std::env::var("JAVA_HOME") {
        Ok(home) => format!(
            "{home}/bin/{name}{}",
            if cfg!(windows) { ".exe" } else { "" }
        ),
        Err(_) => name.to_string(),
    }
}

/// Run one tool with `args`, returning a readable error with stderr on
/// failure.
fn run_tool(program: &str, args: &[&str]) -> Result<(), String> {
    let output = std::process::Command::new(program)
        .args(args)
        .output()
        .map_err(|e| format!("could not run `{program}`: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "`{program} {}` failed:\n{}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    // 1) Write + compile the Java source.
    let classes = "target/userdata-classes";
    let src = "target/userdata-src/com/example/Counter.java";
    std::fs::create_dir_all("target/userdata-src/com/example")
        .map_err(|e| format!("cannot create target/userdata-src/com/example: {e}"))?;
    std::fs::write(src, COUNTER_JAVA).map_err(|e| format!("cannot write {src}: {e}"))?;
    let javac = tool("javac");
    println!("compiling Counter.java with `{javac}` ...");
    run_tool(&javac, &["-d", classes, src])?;

    // 2) Build the JVM with the compiled classes on the class path.
    let java = Java::builder()
        .class_path(classes)
        .build()
        .map_err(|e| e.to_string())?;
    println!("JVM started (class path: {classes})");

    // 3) Register the three natives on com.example.Counter.
    let clazz = java.class("com.example.Counter").map_err(|e| e.to_string())?;
    clazz
        .register_natives(&[
            // `create` returns a concrete class (`Counter`): the type-derived
            // form derives `()Ljava/lang/Object;` (R = JObject), and
            // register_natives resolves the exact return type via reflection
            // at registration time. `increment`/`value` are instance natives
            // whose derived signatures `(J)J` / `()J` match exactly.
            native!("create", counter_create).map_err(|e| e.to_string())?,
            native_inst!("increment", counter_increment).map_err(|e| e.to_string())?,
            native_inst!("value", counter_value).map_err(|e| e.to_string())?,
        ])
        .map_err(|e| e.to_string())?;
    println!("registered create/increment/value natives on com.example.Counter");

    // 4) Create a counter and drive it. The shell/state split: the Java
    //    object carries no data; the Rust state is registered under the
    //    object's identity hash (System.identityHashCode).
    let counter: JObject = clazz
        .call_static("create", ())
        .map_err(|e| e.to_string())?;
    let key: i32 = java
        .class("java.lang.System")
        .map_err(|e| e.to_string())?
        .call_static("identityHashCode", (counter.clone(),))
        .map_err(|e| e.to_string())?;
    println!("Counter shell created; Rust state registered under identity hash {key}");

    let v: i64 = counter
        .call("increment", (5_i64,))
        .map_err(|e| e.to_string())?;
    println!("increment(5) -> {v}");
    let v: i64 = counter
        .call("increment", (3_i64,))
        .map_err(|e| e.to_string())?;
    println!("increment(3) -> {v}");
    let v: i64 = counter.call("value", ()).map_err(|e| e.to_string())?;
    println!("value()      -> {v}");
    assert_eq!(v, 8, "the count persisted across calls — state lives in Rust");
    println!("ok: state persisted across native calls — it lives in Rust, not Java");
    Ok(())
}
