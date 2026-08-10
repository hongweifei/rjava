//! # Typed class bindings (`bind!`) — chains, statics, fields, aliases
//!
//! [`rjava::bind!`](macro@rjava::bind) is the typed layer over the dynamic
//! JNI calls: declare a Java class once with Rust types and every call is a
//! typed method — no `JValue` plumbing, no signature strings.
//!
//! Two parts:
//!
//! 1. **JDK classes** — `StringBuilder` shows instance methods that return
//!    the same class (`-> Self`) so calls chain, plus typed `i32` / `String`
//!    returns; `Math` shows typed `static fn` calls.
//! 2. **A fixture class** (`com.example.BindDemo`, embedded below and
//!    compiled with `javac` at runtime, mirroring `examples/userdata.rs`)
//!    shows the `field` declarations — typed getters/setters over a Java
//!    field — and the `[java_name = "…"]` alias, where the Rust method keeps
//!    an idiomatic name while the JNI call targets a differently named Java
//!    method.
//!
//! No feature flags needed: `bind!` is always available.
//!
//! Run with: `cargo run --example bind`

use rjava::prelude::*;

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

// A JDK class with an instance method that returns the same class
// (`-> Self`), a primitive return and a `String` return.
rjava::bind! {
    "java.lang.StringBuilder" => StringBuilder {
        fn append(s: &str) -> Self;
        fn length() -> i32;
        fn toString() -> String;
    }
}

// A JDK class with static methods (typed static calls).
rjava::bind! {
    "java.lang.Math" => Math {
        static fn max(a: i32, b: i32) -> i32;
    }
}

// The fixture class: a `field` declaration (typed getters/setters over the
// Java field `count`) and a `[java_name = "…"]` alias — the Rust method is
// `doubled`, the Java method is `doubleIt`.
rjava::bind! {
    "com.example.BindDemo" => BindDemo {
        field count: i64;
        fn add(by: i64) -> i64;
        fn doubled() -> i64 [java_name = "doubleIt"];
    }
}

// ---------------------------------------------------------------------------
// The Java source, embedded so the example is one self-contained file
// ---------------------------------------------------------------------------

/// `com.example.BindDemo` — a small bean-ish class with a private `count`,
/// bean accessors and two extra methods. The `count` field is private on
/// purpose: JNI field access ignores Java visibility, so the `field`
/// declaration reads/writes it directly.
const BIND_DEMO_JAVA: &str = r#"
package com.example;

public class BindDemo {
    private long count;

    public BindDemo(long count) {
        this.count = count;
    }

    public long getCount() {
        return count;
    }

    public void setCount(long v) {
        this.count = v;
    }

    /** Increments the count and returns the new value. */
    public long add(long by) {
        this.count += by;
        return this.count;
    }

    /** The Java method behind the `[java_name = "doubleIt"]` alias. */
    public long doubleIt() {
        return this.count * 2;
    }
}
"#;

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
    // 1) Write + compile the fixture source.
    let classes = "target/bind-demo-classes";
    let src = "target/bind-demo-src/com/example/BindDemo.java";
    std::fs::create_dir_all("target/bind-demo-src/com/example")
        .map_err(|e| format!("cannot create target/bind-demo-src/com/example: {e}"))?;
    std::fs::write(src, BIND_DEMO_JAVA).map_err(|e| format!("cannot write {src}: {e}"))?;
    let javac = tool("javac");
    println!("compiling BindDemo.java with `{javac}` ...");
    run_tool(&javac, &["-d", classes, src])?;

    // 2) Build the JVM with the compiled classes on the class path.
    let java = Java::builder()
        .class_path(classes)
        .build()
        .map_err(|e| e.to_string())?;
    println!("JVM started (class path: {classes})");

    // -----------------------------------------------------------------------
    // Part A — JDK classes: `-> Self` chaining and typed statics.
    // -----------------------------------------------------------------------
    let sb = java
        .new::<StringBuilder>(("Hello",))
        .map_err(|e| e.to_string())?;
    let sb = sb.append(" world").map_err(|e| e.to_string())?; // -> Self: chains
    let s: String = sb.toString().map_err(|e| e.to_string())?;
    let len: i32 = sb.length().map_err(|e| e.to_string())?;
    println!("StringBuilder chain: append(\" world\") -> \"{s}\" (length {len})");

    let m: i32 = Math::max(&java, 3, 7).map_err(|e| e.to_string())?;
    let m2: i32 = Math::max(&java, -1, -5).map_err(|e| e.to_string())?;
    println!("Math.max(3, 7) = {m}, Math.max(-1, -5) = {m2}");

    // -----------------------------------------------------------------------
    // Part B — the fixture: `field` accessors and a `[java_name]` alias.
    // -----------------------------------------------------------------------
    let demo = java.new::<BindDemo>((10_i64,)).map_err(|e| e.to_string())?;
    println!("new BindDemo(10)");
    println!(
        "  field count: get_count()           -> {}",
        demo.get_count().map_err(|e| e.to_string())?
    );
    demo.set_count(15).map_err(|e| e.to_string())?;
    println!(
        "  field count: set_count(15)         -> {}",
        demo.get_count().map_err(|e| e.to_string())?
    );
    let v: i64 = demo.add(5).map_err(|e| e.to_string())?;
    println!("  add(5)                           -> {v}");
    let d: i64 = demo.doubled().map_err(|e| e.to_string())?;
    println!("  doubled() [java_name = \"doubleIt\"] -> {d}");
    Ok(())
}
