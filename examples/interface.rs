//! # Rust implements Java interfaces (feature `interface`)
//!
//! The [`interface`](rjava::interface) feature is rjava's "implement a Java
//! interface from Rust" layer — the JDK's `java.lang.reflect.Proxy` plus one
//! fixed precompiled shell class; **zero user-written Java implementation
//! classes**. Two paths, both driven through the same Java host class
//! (`com.example.GreeterHost`, embedded and compiled with `javac` at
//! runtime like `examples/userdata.rs`):
//!
//! 1. **Typed (`interface!` + `proxy_typed`)** — declare the interface once
//!    with Rust types; the macro generates a `trait`; implement it for a
//!    state struct (captured state!). Default methods are auto-forwarded to
//!    their Java default implementation. Use this for anything structured.
//! 2. **Dynamic (`interface::proxy`)** — one closure dispatches on the
//!    method name (and `call.param_types` for overloads). Use this for
//!    quick one-off adapters or when the call shapes are too irregular for
//!    a typed trait.
//!
//! Requires the `interface` feature (see `Cargo.toml`):
//!
//! Run with: `cargo run --example interface --features interface`

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use rjava::interface;
use rjava::jni::JValueOwned;
use rjava::prelude::*;

// ---------------------------------------------------------------------------
// The typed interface! declaration
// ---------------------------------------------------------------------------

// Declare the Java interface once with Rust types: the macro generates
// `pub trait Greeter`. `shout` is deliberately NOT declared — it is a
// `default` method, so the library auto-forwards it to the Java default
// implementation and the trait (and handler) is never consulted.
rjava::interface! {
    "com.example.Greeter" => Greeter {
        fn greet(name: String) -> String;
        fn add(a: i32, b: i32) -> i32;
    }
}

/// The typed path's state: a captured `prefix` baked into every greeting,
/// and a shared call counter (read back in Rust to prove state persists).
struct PoliteGreeter {
    prefix: String,
    calls: Arc<AtomicI64>,
}

impl Greeter for PoliteGreeter {
    fn greet(&self, _env: &mut jni::Env, name: String) -> JavaResult<String> {
        Ok(format!("{}{name}", self.prefix))
    }
    fn add(&self, _env: &mut jni::Env, a: i32, b: i32) -> JavaResult<i32> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(a + b)
    }
}

// ---------------------------------------------------------------------------
// The Java sources, embedded so the example is one self-contained file
// ---------------------------------------------------------------------------

/// `com.example.Greeter` — the interface Rust implements. `shout` is a
/// `default` method: the library auto-forwards it to this Java body.
const GREETER_JAVA: &str = r#"
package com.example;

public interface Greeter {
    String greet(String name);
    int add(int a, int b);

    default String shout(String s) {
        return s.toUpperCase();
    }
}
"#;

/// `com.example.GreeterHost` — Java-side callers that consume a
/// Rust-implemented `Greeter` like any other Java object.
const GREETER_HOST_JAVA: &str = r#"
package com.example;

public class GreeterHost {
    public static String greet(Greeter g, String name) {
        return g.greet(name);
    }

    public static String shout(Greeter g, String s) {
        return g.shout(s);
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
    // 1) Write + compile the Java sources.
    let classes = "target/interface-demo-classes";
    let src = "target/interface-demo-src/com/example";
    std::fs::create_dir_all(src).map_err(|e| format!("cannot create {src}: {e}"))?;
    std::fs::write(format!("{src}/Greeter.java"), GREETER_JAVA)
        .map_err(|e| format!("cannot write Greeter.java: {e}"))?;
    std::fs::write(format!("{src}/GreeterHost.java"), GREETER_HOST_JAVA)
        .map_err(|e| format!("cannot write GreeterHost.java: {e}"))?;
    let javac = tool("javac");
    println!("compiling Greeter.java + GreeterHost.java with `{javac}` ...");
    run_tool(
        &javac,
        &[
            "-d",
            classes,
            &format!("{src}/Greeter.java"),
            &format!("{src}/GreeterHost.java"),
        ],
    )?;

    // 2) Build the JVM with the compiled classes on the class path.
    let java = Java::builder()
        .class_path(classes)
        .build()
        .map_err(|e| e.to_string())?;
    println!("JVM started (class path: {classes})");
    let host = java.class("com.example.GreeterHost").map_err(|e| e.to_string())?;

    // -----------------------------------------------------------------------
    // Path 1 — typed `interface!` + `proxy_typed`: the generated trait is
    // implemented by a state struct (captured `prefix` + call counter), and
    // `proxy_typed` builds the generic handler behind the scenes.
    // -----------------------------------------------------------------------
    println!("== typed interface! + proxy_typed ==");
    let calls = Arc::new(AtomicI64::new(0));
    let proxy = interface::proxy_typed::<dyn Greeter + Send + Sync>(
        Arc::new(PoliteGreeter {
            prefix: "Hello, ".to_string(),
            calls: Arc::clone(&calls),
        }),
        &["com.example.Greeter"],
    )
    .map_err(|e| e.to_string())?;

    // The proxy is an ordinary Java object: Java calls it through the host,
    // and Rust can call it directly too (proxy.call dispatches through the
    // generated trait).
    let s: String = host
        .call_static("greet", (&proxy, "world"))
        .map_err(|e| e.to_string())?;
    let n: i32 = proxy.call("add", (20, 22)).map_err(|e| e.to_string())?;
    let shouted: String = host
        .call_static("shout", (&proxy, "rust"))
        .map_err(|e| e.to_string())?;
    println!("  GreeterHost.greet(proxy, \"world\") -> {s:?}");
    println!("  proxy.add(20, 22) [Rust-side call]  -> {n}");
    println!("  GreeterHost.shout(proxy, \"rust\")   -> {shouted:?}");
    println!(
        "  captured state: add() called {} times",
        calls.load(Ordering::SeqCst)
    );

    // -----------------------------------------------------------------------
    // Path 2 — dynamic `interface::proxy` with a Call-matching closure:
    // one closure implements every method, dispatching on `call.name` (and
    // `call.param_types` — the "dynamic/quick" alternative).
    // -----------------------------------------------------------------------
    println!("== dynamic interface::proxy(Handler) ==");
    let dynamic = interface::proxy(
        Arc::new(|env, call| {
            let mut args = call.args.into_iter();
            match call.name.as_str() {
                "greet" => {
                    let who = String::from_java(env, args.next().expect("greet takes 1 arg"))?;
                    let out = JValueOwned::Object(env.new_string(format!("Hi, {who}"))?.into());
                    Ok(interface::box_value(env, out)?)
                }
                // Overloads are distinguished via `call.param_types`
                // (binary names — `"int"` here).
                "add" if call.param_types == ["int", "int"] => {
                    let a = i32::from_java(env, args.next().expect("add takes 2 args"))?;
                    let b = i32::from_java(env, args.next().expect("add takes 2 args"))?;
                    Ok(interface::box_value(env, JValueOwned::Int(a + b))?)
                }
                _ => Err(JavaError::InvalidArgument("Greeter has no such method")),
            }
        }),
        &["com.example.Greeter"],
    )
    .map_err(|e| e.to_string())?;

    let s: String = host
        .call_static("greet", (&dynamic, "dyn"))
        .map_err(|e| e.to_string())?;
    let n: i32 = dynamic.call("add", (40, 2)).map_err(|e| e.to_string())?;
    let shouted: String = host
        .call_static("shout", (&dynamic, "dyn"))
        .map_err(|e| e.to_string())?;
    println!("  GreeterHost.greet(dynamic, \"dyn\") -> {s:?}");
    println!("  dynamic.add(40, 2) [Rust-side call] -> {n}");
    println!("  GreeterHost.shout(dynamic, \"dyn\")  -> {shouted:?}");
    Ok(())
}
