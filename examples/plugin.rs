//! # The plugin workflow, end to end
//!
//! This example is a single-file demo of runtime plugin loading. It:
//!
//! 1. writes three small Java sources into `target/plugin-demo/src` and
//!    compiles them with `javac` (via `JAVA_HOME/bin/javac`),
//! 2. jars the **API** classes into `target/plugin-demo/api.jar` and the
//!    **plugin** class into `target/plugin-demo/plugin.jar`,
//! 3. builds a JVM with an **empty** system class path,
//! 4. loads `api.jar` at runtime with [`Java::load_jar`], looks up the API's
//!    `Bridge` class, and registers two Rust implementations for its `native`
//!    methods ([`JClass::register_natives`] + [`native!`]),
//! 5. loads `plugin.jar` with a loader whose **parent** is the API loader
//!    ([`Java::class_loader_with_parent`]) — that is what lets plugin code
//!    resolve the API interface and the Rust-backed `Bridge`,
//! 6. instantiates the plugin and calls its `name()` method.
//!
//! # The contract
//!
//! * **API authors** write interfaces + a `Bridge` class declaring `native`
//!   methods and ship the **API jar**. Plugin developers compile against it —
//!   it is their compile-time contract; they never see the Rust side.
//! * **The host** loads the same API jar at runtime, injects the Rust
//!   implementations via `register_natives`, and loads each plugin jar with
//!   the API loader as parent.
//! * **Plugins** are ordinary Java compiled against the API jar; when they
//!   call `Bridge.rustEcho(...)`, the call lands in Rust.
//!
//! Run with: `cargo run --example plugin`

use rjava::prelude::*;
use rjava::native;

// ---------------------------------------------------------------------------
// The Java sources, embedded so the example is one self-contained file
// ---------------------------------------------------------------------------

/// `com.example.plugin.api.Plugin` — the API interface.
const API_PLUGIN_JAVA: &str = r#"
package com.example.plugin.api;

public interface Plugin {
    String name();
}
"#;

/// `com.example.plugin.api.Bridge` — the API's native methods.
const API_BRIDGE_JAVA: &str = r#"
package com.example.plugin.api;

public class Bridge {
    public static native String rustEcho(String s);
    public static native int rustCount(String s);
}
"#;

/// `com.example.plugin.HelloPlugin` — a plugin compiled against the API jar.
const HELLO_PLUGIN_JAVA: &str = r#"
package com.example.plugin;

public class HelloPlugin implements com.example.plugin.api.Plugin {
    public String name() {
        return "HelloPlugin("
            + com.example.plugin.api.Bridge.rustEcho("hi") + ", "
            + com.example.plugin.api.Bridge.rustCount("hi") + " bytes)";
    }
}
"#;

// ---------------------------------------------------------------------------
// The Rust implementations injected into the Bridge
// ---------------------------------------------------------------------------

fn rust_echo(_env: &mut jni::Env, (s,): (String,)) -> JavaResult<String> {
    Ok(format!("{s} from rust"))
}

fn rust_count(_env: &mut jni::Env, (s,): (String,)) -> JavaResult<i32> {
    Ok(s.len() as i32) // byte length
}

// ---------------------------------------------------------------------------
// javac/jar helpers (JAVA_HOME/bin/javac(.exe), like the integration tests)
// ---------------------------------------------------------------------------

/// `javac`/`jar` from `JAVA_HOME/bin` (falling back to `PATH`).
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
    let base = "target/plugin-demo";
    let src = format!("{base}/src");
    let classes = format!("{base}/classes");
    let api_jar = format!("{base}/api.jar");
    let plugin_jar = format!("{base}/plugin.jar");

    // 1) Write + compile the Java sources.
    let api_dir = format!("{src}/com/example/plugin/api");
    let hello_dir = format!("{src}/com/example/plugin");
    std::fs::create_dir_all(&api_dir).map_err(|e| format!("cannot create {api_dir}: {e}"))?;
    std::fs::create_dir_all(&hello_dir).map_err(|e| format!("cannot create {hello_dir}: {e}"))?;
    std::fs::create_dir_all(&classes).map_err(|e| format!("cannot create {classes}: {e}"))?;
    std::fs::write(format!("{api_dir}/Plugin.java"), API_PLUGIN_JAVA)
        .map_err(|e| format!("cannot write Plugin.java: {e}"))?;
    std::fs::write(format!("{api_dir}/Bridge.java"), API_BRIDGE_JAVA)
        .map_err(|e| format!("cannot write Bridge.java: {e}"))?;
    std::fs::write(format!("{hello_dir}/HelloPlugin.java"), HELLO_PLUGIN_JAVA)
        .map_err(|e| format!("cannot write HelloPlugin.java: {e}"))?;

    let javac = tool("javac");
    let jar = tool("jar");
    println!("compiling the API and plugin sources with `{javac}` ...");
    run_tool(
        &javac,
        &[
            "-d",
            &classes,
            &format!("{api_dir}/Plugin.java"),
            &format!("{api_dir}/Bridge.java"),
            &format!("{hello_dir}/HelloPlugin.java"),
        ],
    )?;
    println!("packaging api.jar and plugin.jar ...");
    run_tool(&jar, &["cf", &api_jar, "-C", &classes, "com/example/plugin/api"])?;
    // Only HelloPlugin.class goes into the plugin jar: jarring the whole
    // `com/example/plugin` tree would embed the api classes too, giving the
    // plugin loader its own (native-less) Bridge copy.
    run_tool(
        &jar,
        &["cf", &plugin_jar, "-C", &classes, "com/example/plugin/HelloPlugin.class"],
    )?;

    // 2) Build the JVM with an EMPTY system class path: everything the plugin
    //    workflow touches is loaded at runtime via URLClassLoader.
    let java = Java::builder().build().map_err(|e| e.to_string())?;
    println!("JVM started (system class path empty — all jars load at runtime)");

    // 3) Load the API jar and inject the Rust natives into its Bridge.
    let api = java.load_jar(&api_jar).map_err(|e| e.to_string())?;
    let bridge = api
        .load_class("com.example.plugin.api.Bridge")
        .map_err(|e| e.to_string())?;
    bridge
        .register_natives(&[
            // Type-derived form: the JNI signature is derived from the Rust
            // types — no signature string.
            native!("rustEcho", rust_echo).map_err(|e| e.to_string())?,
            // Explicit-signature form (kept here for illustration): a unique
            // trampoline per registration — the escape hatch when two natives
            // share a Rust signature.
            native!("rustCount", "(Ljava/lang/String;)I", rust_count)
                .map_err(|e| e.to_string())?,
        ])
        .map_err(|e| e.to_string())?;
    println!("registered rustEcho/rustCount natives on the runtime-loaded Bridge");

    // 4) Load the plugin jar with the API loader as parent and use it.
    let plugin = java
        .class_loader_with_parent(&[&plugin_jar], &api)
        .map_err(|e| e.to_string())?;
    let hello = plugin
        .load_class("com.example.plugin.HelloPlugin")
        .map_err(|e| e.to_string())?;
    let obj = hello.new_instance(()).map_err(|e| e.to_string())?;
    let name: String = obj.call("name", ()).map_err(|e| e.to_string())?;
    println!("plugin loaded at runtime, name() = {name}");

    // 5) Close is explicit (Drop only releases the global ref); the JVM GCs
    //    the loaders once nothing references them.
    api.close().map_err(|e| e.to_string())?;
    plugin.close().map_err(|e| e.to_string())?;
    println!("closed both class loaders");
    Ok(())
}
