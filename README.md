# rjava

[![crates.io](https://img.shields.io/crates/v/rjava)](https://crates.io/crates/rjava)
[![docs.rs](https://img.shields.io/docsrs/rjava)](https://docs.rs/rjava/latest/rjava/)
[![License](https://img.shields.io/crates/l/rjava)](LICENSE-MIT)

Safe, ergonomic **Java interop for Rust**, modeled after
[mlua](https://github.com/mlua-rs/mlua). `rjava` is to the JNI what `mlua`
is to the Lua C API: a small, safe wrapper — you never touch raw JNI, never
write JNI signature strings, and never manage JVM attachment by hand, and
there is **no `unsafe`** in `rjava`'s own code
(`#![forbid(unsafe_code)]`). It is built on top of the
[`jni`](https://crates.io/crates/jni) crate (jni-rs).

## Table of contents

- [What is rjava](#what-is-rjava) · 这是什么
- [Feature highlights](#feature-highlights) · 功能亮点
- [Quick start](#quick-start) · 快速开始
- [bind! class bindings](#bind-class-bindings) · bind! 类绑定
- [interface — Java interfaces from Rust](#interface--java-interfaces-from-rust) · interface 接口实现
- [serde ↔ Map & JavaBean](#serde--map--javabean) · serde 与 JavaBean
- [Async — the four paths](#async--the-four-paths) · 异步：四条路径
- [Examples](#examples) · 示例
- [Feature notes & limitations](#feature-notes--limitations) · 功能说明与限制
- [Development](#development) · 开发
- [Benchmarks](#benchmarks) · 基准测试
- [中文说明](#中文说明)

## What is rjava

`rjava` is a small, safe, ergonomic wrapper for calling Java from Rust and
Rust from Java. Classes, objects, static members, arrays, and `main` entry
points become plain Rust calls; a thrown Java exception becomes a typed
Rust error, captured and cleared before the call returns; and Rust
functions — fn items *or* closures — register as Java `native` methods
without signature strings. You never manage JVM attachment by hand, and
threads spawned from Rust work with no extra ceremony.

The design is modeled on mlua, so the concepts map one-to-one:

| mlua             | rjava                              |
|------------------|------------------------------------|
| `Lua`            | `Java` (JVM facade)                |
| `IntoLua`        | `ToJava` (Rust → JNI arguments)    |
| `FromLua`        | `FromJava` (JNI value → Rust)      |
| `MultiValue`     | tuples (`()`, `(a, b)`, … ≤ 64)    |
| `LuaError`       | `JavaError` (Display + operation context) |
| `Lua::create_function` | `JClass::register_natives` + `native!` / `native_inst!` |
| `UserData`       | `rjava::userdata` (`bind` / `get` / `with` / `unbind` / `create_shell`) + `native_inst!` |

**No `unsafe` in your crate.** All FFI lives inside `rjava` (the two raw
JNI calls behind `RegisterNatives` sit in the `rjava-helper` crate); the
macro *expansions* are `unsafe`-free, so a crate that uses `rjava` can
keep `#![forbid(unsafe_code)]` — the integration tests do exactly that.

## Feature highlights

Each feature is documented in depth on
[docs.rs](https://docs.rs/rjava/latest/rjava/):

- **`bind!`** — compile-time-typed class bindings: declare a class and its methods once, call with direct typed args — [macro.bind](https://docs.rs/rjava/latest/rjava/macro.bind.html) · [rjava::bind](https://docs.rs/rjava/latest/rjava/bind/)
- **`interface`** — implement Java interfaces from Rust: zero user-written Java implementation classes — the JDK's `Proxy` + one fixed rjava shell class (optional `interface` feature) — [rjava::interface](https://docs.rs/rjava/latest/rjava/interface/)
- **JVM facade** — create, configure, and reuse the process JVM (`Java::builder()`, `from_env`) — [struct.Java](https://docs.rs/rjava/latest/rjava/struct.Java.html)
- **`ToJava` / `FromJava`** — tuples ≤ 64, primitives, `String`, `Option<T>`, `Vec<T>` ⇄ Java arrays — [trait.ToJava](https://docs.rs/rjava/latest/rjava/trait.ToJava.html) · [trait.FromJava](https://docs.rs/rjava/latest/rjava/trait.FromJava.html)
- **Native methods** — register Rust functions (fn items *or* closures) as Java `native` methods; type-derived signatures, explicit-signature escape hatch — [macro.native](https://docs.rs/rjava/latest/rjava/macro.native.html) · [macro.native_inst](https://docs.rs/rjava/latest/rjava/macro.native_inst.html)
- **userdata** — Rust-backed Java objects, identity-keyed, auto-released on GC; binding from the constructor ("direct `new`"), and `userdata::with` to borrow the bound state in a closure — [rjava::userdata](https://docs.rs/rjava/latest/rjava/userdata/)
- **Plugin jars** — load API/plugin jars at runtime over `java.net.URLClassLoader` (API jar = compile-time contract) — [struct.JClassLoader](https://docs.rs/rjava/latest/rjava/struct.JClassLoader.html)
- **serde ↔ `Map`** — Rust structs ⇄ `java.util.HashMap` / `ArrayList` value trees (feature `serde`) — [rjava::serde](https://docs.rs/rjava/latest/rjava/serde/)
- **JavaBean** — Rust structs ⇄ plain Java beans via getter/setter reflection (feature `serde`) — [rjava::bean](https://docs.rs/rjava/latest/rjava/bean/)
- **Async bridge — four paths** — await a Java `CompletableFuture` as a std-only Rust `Future` ([rjava::future](https://docs.rs/rjava/latest/rjava/future/)), register Rust **async** functions as Java natives returning `CompletableFuture`s ([macro.async_native](https://docs.rs/rjava/latest/rjava/macro.async_native.html)), turn Java listener callbacks into awaitable futures ([rjava::rx](https://docs.rs/rjava/latest/rjava/rx/), feature `interface`), and call methods asynchronously ([Java::call_async](https://docs.rs/rjava/latest/rjava/struct.Java.html#method.call_async)) — all std-only, executor-agnostic
- **`run_main`** — invoke a Java `public static void main(String[])` entry point — [Java::run_main](https://docs.rs/rjava/latest/rjava/struct.Java.html#method.run_main)
- **Arrays** — `JArray<T>`, `Vec<T>` ⇄ `int[]` / `String[]` / `Object[]`, null-tolerant object arrays — [struct.JArray](https://docs.rs/rjava/latest/rjava/struct.JArray.html)
- **Autoboxing / unboxing** — primitives box for `Object` parameters; wrappers unbox for primitive parameters (exact type, no widening) — [crate docs](https://docs.rs/rjava/latest/rjava/)
- **Benchmarks** — criterion harness measured against a real JVM; headline numbers below — [crate docs](https://docs.rs/rjava/latest/rjava/)

## Quick start

```rust,no_run
use rjava::prelude::*;

fn main() -> JavaResult<()> {
    // 1) JVM facade (mlua's `Lua` analog)
    let java = Java::builder()
        .class_path("target/classes")   // optional; ";"-separated on Windows
        .option("-Xmx256m")             // raw JVM option, repeatable
        .build()?;                      // create JVM via the invocation API

    // (Rust loaded inside a Java process: attach from a native method)
    // let java = Java::from_env(&mut env)?;   // env: &mut jni::Env

    // 2) Classes & objects
    let clazz: JClass = java.class("java.lang.StringBuilder")?;
    let sb: JObject = clazz.new_instance(("Hello",))?; // ctor args as tuple
    let len: i32 = sb.call("length", ())?;
    sb.call_void("append", (" world",))?;              // void methods: call_void
    let s: String = sb.call("toString", ())?;
    let rt: JClass = sb.class()?;                      // runtime class
    let sb2: JObject = java.new_object("java.lang.StringBuilder", ("Hi",))?;

    // 3) Static members
    let max: i32 = java.class("java.lang.Math")?.call_static("max", (3_i32, 7_i32))?;
    let pi: f64 = java.class("java.lang.Math")?.get_static_field("PI")?;

    // 4) Arrays
    let arr: JArray<i32> = java.new_array(10)?;        // int[10]
    arr.set(0, 42)?;
    let v: i32 = arr.get(0)?;
    let ints: JArray<i32> = java.new_array_from(vec![1, 2, 3])?; // from a Vec
    let objs: JArray<JObject> = java.new_object_array("java.lang.String", 3)?;
    objs.set(0, java.new_object("java.lang.String", ("a",))?)?;
    let maybe: Option<JObject> = objs.get(1)?;         // null element -> None

    // 5) Errors: a thrown Java exception becomes a typed Rust error.
    //    Dynamic-call failures name the failing operation (`WithContext`),
    //    and `Display` renders like the Java exception itself:
    let err = java
        .class("java.lang.Integer")?
        .call_static::<_, i32>("parseInt", ("x",))
        .expect_err("parseInt(\"x\") must fail");
    assert_eq!(
        err.to_string(),
        "calling parseInt(String) on java.lang.Integer: \
         java.lang.NumberFormatException: For input string: \"x\""
    );
    Ok(())
}
```

`JavaError` prints like Java: `println!("{e}")` renders
`java.lang.NumberFormatException: For input string: "x"` (the derived `Debug`
form is unchanged), and dynamic-call failures name the operation via
`JavaError::with_operation`.

Run it with a JDK on `PATH` (or `JAVA_HOME` set), then
`cargo run --example basic`. Full documentation:
<https://docs.rs/rjava/latest/rjava/>.

## bind! class bindings

The dynamic path — `java.new_object("java.lang.StringBuilder", ("Hello",))?`
then `sb.call("length", ())?` — is string-typed and runtime-checked. For a
class you use a lot, `bind!` declares the class and its methods **once**,
with Rust types, and every call becomes a direct typed method:

```rust,no_run
use rjava::bind;
use rjava::prelude::*;

bind! {
    "java.lang.StringBuilder" => StringBuilder {
        fn append(s: &str) -> Self;             // -> Self re-wraps: chainable
        fn length() -> i32;
        fn toString() -> String;
    }
}

bind! {
    "java.lang.Math" => Math {
        static fn max(a: i32, b: i32) -> i32;   // static fn: &Java first
    }
}

fn demo(java: &Java) -> JavaResult<()> {
    let sb = java.new::<StringBuilder>(("Hello",))?; // ctor args as tuple
    sb.append(" world")?;
    let len: i32 = sb.length()?;
    let hi: i32 = Math::max(java, 3, 7)?;            // static: java first
    Ok(())
}
```

- **Construction** — `java.new::<T>(args)` (constructor) or
  `java.wrap::<T>(obj)` (wrap an existing handle; no JNI call, and the class
  resolves lazily on the first real call).
- **Dynamic escape hatch** — a wrapper is not sealed: `JavaBound::obj`
  borrows the underlying `JObject`, so typed and dynamic
  (`call` / `call_void` / `get_field`) calls mix freely on the same object;
  `into_obj` consumes the wrapper back into the handle.
- **Cost** — the class reference is cached once per wrapper (`OnceLock` over
  a JNI global reference, no `unsafe`); method IDs are re-derived per call by
  design, since jni-rs's `MethodID` lifetime makes caching them unsafe.
- **Fields** — `field name: Type;` binds a Java field as a typed accessor
  pair: `field label: String;` generates `get_label()` / `set_label(v)` over
  the Java field `label` (`static field MAGIC: i32;` → `get_static_magic(java)`
  / `set_static_magic(java, v)`). A `bool` field's getter falls back to the
  bean-style `get<Name>()` / `is<Name>()` accessor methods when the field
  itself doesn't exist.
- **Java-name aliases** — `fn to_string() -> String [java_name = "toString"];`
  calls the Java method `toString` while the Rust method keeps the idiomatic
  `to_string` name (only the JNI target changes; the alias works in
  `interface!` too).

Full reference: [macro.bind](https://docs.rs/rjava/latest/rjava/macro.bind.html)
· [rjava::bind](https://docs.rs/rjava/latest/rjava/bind/).

## interface — Java interfaces from Rust

The `interface` feature (optional, off by default) is the mlua
`create_function` analog for Java: **implement Java interfaces from Rust
with zero user-written Java implementation classes.** The JDK's own
`java.lang.reflect.Proxy` generates the implementation at runtime; rjava
ships exactly **one fixed precompiled shell class** (an
`InvocationHandler`, `javac --release 8`, class-file version 52,
**committed** at `interface/java/rjava/shell/InvocationHandlerShell.class`
and embedded via `include_bytes!` — no `javac` needed to build). At first
use the shell is
bootstrapped once per process from a per-process temp dir via a
`URLClassLoader` — nothing is generated or compiled at runtime.

```rust,no_run
use rjava::interface;
use rjava::jni::JValueOwned;
use rjava::prelude::*;
use std::sync::Arc;

// Java side — the interface is ordinary Java, nothing rjava-specific:
//   public interface Greeter {
//     String greet(String name);
//     int add(int a, int b);
//     long add(long a, long b);   // overload of add(int,int)
//   }

fn make_greeter(java: &Java) -> JavaResult<JObject> {
    // Rust side — one closure implements every method. Each call arrives as
    // an `interface::Call { name, param_types, args }`: dispatch on the
    // method name and, for overloads, on the declared parameter types. The
    // `Arc<dyn Handler>` parameter makes the closure's signature infer:
    // no lifetime annotations needed.
    interface::proxy(
        Arc::new(|env: &mut jni::Env, call: interface::Call| {
            let mut args = call.args.into_iter();
            match call.name.as_str() {
                "greet" => {
                    let who = String::from_java(env, args.next().expect("greet takes 1 arg"))?;
                    Ok(interface::box_value(env, JValueOwned::Object(env.new_string(format!("Hello, {who}"))?.into()))?)
                }
                "add" if call.param_types == ["int", "int"] => {
                    let a = i32::from_java(env, args.next().expect("add(int,int) takes 2 args"))?;
                    let b = i32::from_java(env, args.next().expect("add(int,int) takes 2 args"))?;
                    Ok(interface::box_value(env, JValueOwned::Int(a + b))?)
                }
                "add" if call.param_types == ["long", "long"] => {
                    let a = i64::from_java(env, args.next().expect("add(long,long) takes 2 args"))?;
                    let b = i64::from_java(env, args.next().expect("add(long,long) takes 2 args"))?;
                    Ok(interface::box_value(env, JValueOwned::Long(a + b))?)
                }
                other => Err(JavaError::InvalidArgument("unexpected method")),
            }
        }),
        &["com.example.Greeter"], // interface binary names
    )
}
```

- **`interface::proxy(Arc<Handler>, &[iface_names])`** — returns the
  JDK-generated proxy object; the `Arc<dyn Handler>` parameter type makes
  the closure's higher-ranked signature infer (no lifetime annotations).
- **Handler** — `dyn Fn(&mut Env, interface::Call) -> JavaResult<JObject>
  + Send + Sync + 'static`, where `Call { name: String, param_types: Vec<String>, args: Vec<JValueOwned> }`:
  dispatch with `match call.name.as_str()`, and distinguish overloads via
  `call.param_types` (`["int","int"]` vs `["long","long"]`). Primitive
  arguments arrive unboxed (`JValueOwned::Int` for an `int`, …) and
  convert with the existing `FromJava` impls.
- **`Object` methods are intercepted** — `toString()`, `hashCode()` and
  `equals(Object)` never reach the handler; the library answers them with
  the JDK's identity defaults (standard `ClassName@hexhash` format string,
  `System.identityHashCode` boxed, reference identity).
- **Exact-signature matching** — interception matches the full declared
  signature, not the name alone: a domain method that merely shares a name
  (e.g. `toString(int)`) still reaches the handler. These three are
  library-reserved in v1.
- **`default` methods auto-forward** — proxied `default` methods are
  dispatched to their Java default implementation via reflective
  `InvocationHandler.invokeDefault` (Java 16+; the shell class stays
  compiled with `--release 8`). On Java < 16 they fall back to the handler.
- **`box_value` / `null`** — build the returned handle: `box_value` boxes
  primitive results into their wrapper classes (unboxed again by the
  proxy's generated code); `null` is the return for `void` methods.
- **State & errors** — the closure's captured state lives in the userdata
  registry (auto-released when the proxy becomes unreachable); a handler
  error becomes a thrown Java exception (handler panics → `RuntimeException`).
- **No build requirement** — the one shell class is precompiled and
  committed, so building with the feature needs **no `javac`**; a
  freshness guard test recompiles the `.java` and fails if the committed
  `.class` goes stale (skipping loudly when `javac` is absent).

### Typed interfaces (`interface!`)

The closure handler above is the **dynamic** path. `interface!` is the
**typed** path — the mirror of `bind!` on the implement side: declare the
interface once with Rust types, the macro generates a Rust trait, implement
it for your state struct, and `interface::proxy_typed` builds the generic
handler behind the scenes:

```rust,no_run
use rjava::interface;
use rjava::prelude::*;
use std::sync::Arc;

rjava::interface! {
    "com.example.Greeter" => Greeter {      // generates: pub trait Greeter
        fn greet(name: String) -> String;
        fn add(a: i32, b: i32) -> i32;
        fn ping();                          // void sugar -> JavaResult<()>
    }
}

struct MyGreeter { prefix: String }

impl Greeter for MyGreeter {
    fn greet(&self, _env: &mut jni::Env, name: String) -> JavaResult<String> {
        Ok(format!("{}{name}", self.prefix))
    }
    fn add(&self, _env: &mut jni::Env, a: i32, b: i32) -> JavaResult<i32> {
        Ok(a + b)
    }
    fn ping(&self, _env: &mut jni::Env) -> JavaResult<()> { Ok(()) }
}

fn make_greeter() -> JavaResult<JObject> {
    interface::proxy_typed::<dyn Greeter + Send + Sync>(
        Arc::new(MyGreeter { prefix: "Hi, ".into() }),
        &["com.example.Greeter"],           // interface binary names
    )
}
```

- Each generated trait method takes `&self`, the `jni::Env`, the declared
  parameters, and returns `JavaResult<R>` — arguments and return convert
  automatically (`FromJava` in, `ToJava` out, no `JValueOwned` handling);
  `fn ping();` is the `void` sugar for `JavaResult<()>`.
- Overloads dispatch on `(name, param_types)`, exactly like the dynamic
  path; `[java_name = "…"]` aliases work here too
  (`fn add_long(a: i64, b: i64) -> i64 [java_name = "add"];`).
- The three library-reserved `Object` methods (`toString()`, `hashCode()`,
  `equals(Object)`) are **compile-time rejected** in `interface!` — they are
  intercepted before the handler, so a trait method with that exact
  signature could never be called.
- `proxy_typed::<dyn Trait + Send + Sync>(Arc::new(state), &[iface_names])`
  — the state is shared across calls (`Send + Sync + 'static`), and one
  state struct can implement several interfaces. The generic
  `proxy(Arc<Handler>, …)` stays for dynamic cases (`static` interface
  methods, ad-hoc dispatch).

Full reference: [rjava::interface](https://docs.rs/rjava/latest/rjava/interface/) ·
[macro.interface](https://docs.rs/rjava/latest/rjava/macro.interface.html).

## serde ↔ Map & JavaBean

The `serde` feature (opt-in, like `interface`) maps Rust structs to both
Java contracts: `java.util.HashMap` / `ArrayList` value trees
(`JavaMap<T>` / `rjava::serde::from_object`) and plain Java beans via
getter/setter reflection (`JavaBean<T>`). Full reference:
[rjava::serde](https://docs.rs/rjava/latest/rjava/serde/) ·
[rjava::bean](https://docs.rs/rjava/latest/rjava/bean/).

- **Java records** — a record reads through its component accessor `x()`
  (after `get<Name>` / `is<Name>`; plain beans are never probed for `x()`)
  and writes through its **canonical constructor** (records have no
  setters). The Rust struct's field order must match the Java component
  order — checked at runtime, mismatches error loudly naming both orders.
  Records document **Java 16 as the JVM floor** (`Class.isRecord()`); on an
  older JVM the class is treated as a plain bean.
- **Bean-to-bean nesting** — a struct field typed `JavaBean<Inner>`
  serializes to a nested bean object (built via `new` + setters, or the
  canonical constructor for a record) passed to the property's setter; the
  `__rjava_bean_class` marker carries the class on the write side, and reads
  derive the class from the nested object's runtime class. Through the
  `JavaMap` value tree the same field lands as an ordinary nested `HashMap`
  that round-trips.
- **`char` support** — `char` ⇄ `java.lang.Character` at the value level
  (code points above `U+FFFF` error), with exact `Byte` / `Short` / `Float`
  wrappers (`i8`/`u8` → `Byte`, `i16` → `Short`, `f32` → `Float` — no
  widening). `u8` reads back with the unsigned interpretation of the
  `byte` (`byte` `-1` → `u8` `255`), so every `u8` round-trips;
  `Vec<Vec<T>>` flows both ways as nested `ArrayList`s.

## Async — the four paths

`CompletableFuture` bridges in both directions, all **std-only** (no
executor dependency, no `unsafe`):

1. **`java_future`** — a Java `java.util.concurrent.CompletableFuture` as a
   Rust `Future`: the first poll spawns one detached std thread that blocks
   on `get()`, converts the value with `FromJava`, and wakes the future —
   [rjava::future](https://docs.rs/rjava/latest/rjava/future/).
2. **`async_native!`** — the mirror: a Rust **async** function (fn item or
   closure) registers as a Java static `native` returning
   `CompletableFuture<R>`; each call runs the Rust future on its own std
   thread and completes the Java future —
   [macro.async_native](https://docs.rs/rjava/latest/rjava/macro.async_native.html).
3. **`rx::from_callback`** (feature `interface`) — a Java listener callback
   becomes an awaitable **one-shot** Rust future: hand `from_callback` the
   interface names and a closure that builds the plain `interface::proxy`
   handler (no `interface!` / `proxy_typed` needed), register the returned
   proxy with the Java host, and the future resolves with the first event —
   [rjava::rx](https://docs.rs/rjava/latest/rjava/rx/).
4. **`Java::call_async` / `Java::call_static_async`** — the async analog of
   `call` / `call_static`: the JVM call runs on a worker thread and the
   result arrives through the future —
   [Java::call_async](https://docs.rs/rjava/latest/rjava/struct.Java.html#method.call_async).

The full circle — Rust → Java → Rust:

```rust,no_run
use std::sync::Arc;
use rjava::async_native;
use rjava::future::java_future;
use rjava::interface::{self, Call};
use rjava::prelude::*;
use rjava::rx;

// 1) a Rust async fn registers as the Java native `AsyncNativeDemo.compute`
//    — Java callers see a `CompletableFuture<Integer>`
async fn compute(_java: Java, (a, b): (i32, i32)) -> JavaResult<i32> {
    Ok(a + b)
}

# async fn demo(java: &Java) -> JavaResult<()> {
let clazz = java.class("AsyncNativeDemo")?;
clazz.register_natives(&[async_native!("compute", compute)?])?;

// 2) await a CompletableFuture from Rust: Rust -> Java -> Rust
let cf: JObject = clazz.call_static("compute", (1_i32, 2_i32))?;
let sum: i32 = java_future(java.clone(), cf).await?;
# let _ = sum;

// 3) a Java listener callback as an awaitable future (feature `interface`)
let (listener, event) = rx::from_callback::<String, _>(&["Listener"], |tx| {
    Arc::new(move |env, call: Call| {
        if call.name == "onEvent" {
            let value =
                String::from_java(env, call.args.into_iter().next().expect("onEvent takes 1 arg"))?;
            let _ = tx.send(Ok(value));
        }
        Ok(interface::null())
    })
})?;
let host: JObject = java.new_object("Host", (&listener,))?;
host.call_void("fire", ("hello from Java",))?;
let value: String = event.await?;
# let _ = value;

// 4) async method calls: tokio blocking pool when a runtime is current
let sb: JObject = java.new_object("java.lang.StringBuilder", ())?;
let s: String = java.call_async(&sb, "toString", ()).await?;
# let _ = s;
# Ok(())
# }
```

### Semantics

- **Thread-per-call, no executor.** `java_future` blocks on
  `CompletableFuture.get()` on one detached std thread; each `async_native!`
  call runs its Rust future on its own std thread under
  `futures_lite::future::block_on` — a **real thread-parking waker**, so
  executor-agnostic futures complete (plain computation, other rjava
  bridges, futures that park in `poll` waiting for a wakeup, cross-thread
  channels). With the `tokio` feature, futures that need a tokio runtime
  (timers, IO, tokio channels) run when the native is called inside a tokio
  runtime: the future is spawned on that runtime, falling back to the std
  thread otherwise. `call_async` works the same way: a worker thread per
  call.
- **`tokio` feature (optional, default off).** With `features = ["tokio"]`,
  `call_async` / `call_static_async` dispatch the JVM call through
  `tokio::task::spawn_blocking` when a tokio runtime is current; without a
  runtime (or without the feature) they fall back to a detached std thread —
  enabling the feature never breaks non-tokio users, and the std-only paths
  never require tokio.
- **Errors.** `Err(JavaError::JavaException { class, message })` from an
  `async_native!` future is materialized as an instance of `class` and
  delivered via `completeExceptionally`; any other `Err` and any panic
  complete exceptionally with a `java.lang.RuntimeException` (mirroring the
  synchronous native-method rules). A `java_future` awaiting such a future
  reports the exception through the usual `ExecutionException`-cause unwrap.
- **Registration collisions.** Two different `async_native!` registrations
  with the same Rust argument-tuple type share one trampoline (keyed by `A`
  only — Java always sees a `CompletableFuture` return), and
  `register_natives` rejects the second, exactly like the synchronous path.
  There is **no explicit-signature form for async natives in v1** — give the
  two methods distinct Rust argument-tuple types instead.
- **Cancellation.** Dropping a Rust future never cancels the Java side: an
  `async_native!` worker keeps running (Java-side `cancel()` does not
  propagate in v1), a dropped `call_async` future discards the in-flight
  call's result, and `java_future` cancellation is a Java-side concern
  (`cf.call_void("cancel", (true,))`).
- **One-shot callbacks.** `rx::from_callback` completes with the first
  event; later sends are dropped. Dropping the future does **not**
  unregister the listener: dropped before its first poll it spawns no
  worker (the handler's later `send`s fail observably), dropped after a
  spawn the worker keeps waiting — cancel from the Java side if needed.

## Examples

| Example | What it shows | Run |
|---|---|---|
| `examples/basic.rs` | End-to-end JVM facade demo: constructors, methods, statics, `ArrayList` autoboxing, arrays, typed errors (JDK classes only — no `javac` needed) | `cargo run --example basic` |
| `examples/userdata.rs` | Rust-backed Java objects: embedded `Counter.java` compiled with `javac`, static factory + `create_shell`, identity-keyed state, auto-release | `cargo run --example userdata` |
| `examples/plugin.rs` | Plugin workflow end to end: writes and compiles three Java sources, jars them into `api.jar` / `plugin.jar`, loads them at runtime with `register_natives` + a parent classloader | `cargo run --example plugin` |
| `examples/bind.rs` | Typed class bindings (`bind!`): `StringBuilder` `-> Self` chains, `Math` statics, and a runtime-compiled fixture with `field` accessors and a `[java_name = "…"]` alias | `cargo run --example bind` |
| `examples/interface.rs` | Rust implements Java interfaces (feature `interface`): typed `interface!` + `proxy_typed` with captured state and default-method auto-forward, plus the dynamic `interface::proxy` closure path | `cargo run --example interface --features interface` |
| `examples/async_demo.rs` | The full async circle: `java_future` over `CompletableFuture.supplyAsync` (Rust `Supplier` proxy), `call_async` StringBuilder round trip (tokio blocking pool), `rx::from_callback` listener fired from a Java thread, and `async_native!` registered + awaited back — features `tokio` + `interface` | `cargo run --example async_demo --features "tokio interface"` |

## Feature notes & limitations

* **No unsigned integers.** Java has no `u32`/`u64` (and no `u16`);
  `ToJava`/`FromJava` deliberately omit them. Use `char` for the Java
  `char` type and `Vec<u8>` for `byte[]` (cast through `i8`, documented).
* **No widening unboxing.** A wrapper object matches a primitive parameter
  only when it is the *exact* wrapper type (`Integer` → `int`, …);
  `Number` does not unbox, an `Integer` does not match a `long` parameter,
  and `null` never matches a primitive — annotate the call with the exact
  primitive instead.
* **Parameter limits.** The type-derived form covers argument tuples up to
  64 elements (63 declared parameters for `native_inst!` — the receiver
  takes the 64th slot); the explicit-signature form accepts any signature
  within the JVM's method-descriptor limit of 255 parameter units
  (JVMS §4.3.3).
* **No multi-dimensional arrays in native-method signatures** (v1 scope;
  the array facade covers `Vec<T>` ⇄ `T[]` one level deep).
* **The dependency must be named `rjava`.** The `native!`/`native_inst!`
  expansions reference `::rjava::…` paths, so renaming the dependency
  breaks the macro expansions.
* **JVM reuse.** `Java::builder().build()` reuses a JVM already created
  through this crate. When Rust is embedded in a Java process (e.g. a
  native library loaded by the JVM), attach to the running JVM with
  `Java::from_env` from a native method's `&mut jni::Env`.
* **No native constructors.** Java source forbids `native` on constructors
  (JLS); use a static factory (`userdata::create_shell`) or the
  "direct `new`" pattern — a plain constructor whose body calls a native
  `init()` that binds the state.
* **Plugin model = API jar.** There is no runtime `.class` generation; the
  API jar (interfaces + a `Bridge` class with `native` methods) is the
  compile-time contract plugin developers build against, resolved at
  runtime through a parent `URLClassLoader`.
* **Same-signature collision rule.** Two *different* type-derived
  registrations with the same Rust signature would map to one shared
  trampoline (the second would silently replace the first), so
  `register_natives` rejects them up front; the explicit-signature form
  generates a unique trampoline per registration and is the escape hatch.
* **userdata auto-release.** Bindings hold **weak** global references, so
  a bound object is *not* pinned: when the shell becomes unreachable and
  is garbage-collected, its binding (and the Rust state) is released
  automatically by a background cleaner thread (plus lazy draining on
  every userdata call); `unbind` remains for explicit release.
* The `serde` feature is **opt-in** (`features = ["serde"]`); without it,
  `rjava` carries no serde dependency.
* The `interface` feature is **opt-in** (`features = ["interface"]`). The
  one fixed shell class is precompiled (`javac --release 8`) and
  **committed** at `interface/java/rjava/shell/InvocationHandlerShell.class`,
  embedded via `include_bytes!` — building with the feature needs **no
  `javac`** (no JDK at build time at all); a freshness guard test keeps
  the `.java`/`.class` in sync, skipping loudly when `javac` is absent. At
  runtime the class is bootstrapped once per process from a per-process
  temp dir via a `URLClassLoader` — nothing is generated at runtime.
* The `tokio` feature is **opt-in** (`features = ["tokio"]`): it routes
  `call_async` / `call_static_async` through `tokio::task::spawn_blocking`
  and makes `async_native!` spawn on the current tokio runtime when one is
  in scope; without a runtime (or the feature) the std-thread fallback
  applies — enabling it
  never breaks non-tokio users (see [Async — the four paths](#async--the-four-paths)).

## Development

The repo is a Cargo workspace: `rjava` (root) + `rjava-macros` (proc
macros: `native!` / `native_inst!`) + `rjava-helper` (the unsafe
register helper); `cargo test --workspace` runs all crates. The
repository is hosted on [GitHub](https://github.com/hongweifei/rjava),
and the CI runs on GitHub Actions.

```sh
cargo build            # zero warnings
cargo test             # integration tests run against your real JVM
cargo run --example basic
cargo bench            # criterion benchmarks against a real JVM
cargo doc --no-deps
cargo clippy --all-targets
```

JDK 21 is required — the tests and examples run on a real JVM and compile
Java fixtures with `javac` (a JDK on `PATH` or `JAVA_HOME` set). MSRV is
Rust 1.88 (the crate uses let-chains, stabilized in 1.88).

**CI** (`.github/workflows/ci.yml`, runs on GitHub): a `test` job on a
three-OS matrix (ubuntu / macos-arm64 / windows) with stable Rust and
Temurin JDK 21 — `-D warnings` gates build, clippy (`--all-features`) and
docs; the full test suite runs on the real JVM on all three OSes, and a
follow-up step fails the job if any test silently skipped for lack of a
JVM; the criterion bench harness is compile-gated (`cargo bench --no-run`).
An `msrv` job type-checks every target on the exact 1.88.0 toolchain.

## Benchmarks

A criterion harness (`benches/bench.rs`, `cargo bench`) measures the hot
paths against a real JVM; if no JVM or `javac` is found it prints a loud
skip reason instead of benchmarking garbage. Benchmarks are **not run** on
CI, but the harness is compile-gated there. Two hotspots were fixed in the
optimization round (JDK 21, Windows x64):

* **`Vec<i32>` ⇄ `int[]` conversions** — bulk `get_region`/`set_region`
  instead of one JNI call per element: the 1024-element round trip dropped
  114 µs → 6.0 µs (write) and 143 µs → 5.7 µs (read), ≈ −95%.
* **userdata identity lookup** — `java/lang/System` cached as a
  process-global reference (`OnceLock<Global>`): `userdata::get` dropped
  2.9 µs → 1.6 µs (−45%).

Not hot per the data: the native-call floor (~1.0–1.5 µs), string
conversions (~0.3–0.7 µs), and `String[]` per-element conversion cost
(inherent — reference arrays have no region API).

---

## 中文说明

`rjava` 是一个受 [mlua](https://github.com/mlua-rs/mlua) 启发、构建在
[jni-rs](https://crates.io/crates/jni) 之上的 **安全、易用的 Rust ↔ Java 互操作库**。
库自身没有任何 `unsafe` 代码（`#![forbid(unsafe_code)]`），使用方同样可以保持
`#![forbid(unsafe_code)]`。完整 API 文档在
[docs.rs](https://docs.rs/rjava/latest/rjava/) —— 每个模块都有详解与示例。

### 功能一览

- **`bind!`** — 编译期类型化的类绑定：一次性声明类与方法，之后直接带类型调用 — [macro.bind](https://docs.rs/rjava/latest/rjava/macro.bind.html) · [rjava::bind](https://docs.rs/rjava/latest/rjava/bind/)
- **`interface`** — 从 Rust 实现 Java 接口：零手写 Java 实现类；JDK `Proxy` + 一个固定 rjava shell 类（可选 `interface` feature）— [rjava::interface](https://docs.rs/rjava/latest/rjava/interface/)
- **JVM 门面** — 创建、配置、复用进程内 JVM（`Java::builder()` / `from_env`）— [struct.Java](https://docs.rs/rjava/latest/rjava/struct.Java.html)
- **`ToJava` / `FromJava`** — 元组（≤ 64）、基本类型、`String`、`Option<T>`、`Vec<T>` ⇄ Java 数组 — [trait.ToJava](https://docs.rs/rjava/latest/rjava/trait.ToJava.html) · [trait.FromJava](https://docs.rs/rjava/latest/rjava/trait.FromJava.html)
- **native 方法** — 把 Rust 函数（函数项或闭包）注册为 Java `native` 方法；类型推导签名 + 显式签名逃生门 — [macro.native](https://docs.rs/rjava/latest/rjava/macro.native.html) · [macro.native_inst](https://docs.rs/rjava/latest/rjava/macro.native_inst.html)
- **userdata** — Rust 支撑的 Java 对象：按对象身份索引、GC 后自动释放；也支持在构造器里绑定（"直接 new"）；`userdata::with` 闭包糖用于借用绑定状态 — [rjava::userdata](https://docs.rs/rjava/latest/rjava/userdata/)
- **插件 jar** — 运行期加载 API / 插件 jar（`URLClassLoader`，API jar 即编译期契约）— [struct.JClassLoader](https://docs.rs/rjava/latest/rjava/struct.JClassLoader.html)
- **serde ↔ Map** — Rust 结构体 ⇄ `HashMap` / `ArrayList` 值树（可选 `serde` feature）— [rjava::serde](https://docs.rs/rjava/latest/rjava/serde/)
- **JavaBean** — 经 getter/setter 反射在 Rust 结构体与普通 Java Bean 之间互转（可选 `serde` feature）— [rjava::bean](https://docs.rs/rjava/latest/rjava/bean/)
- **异步桥 — 四条路径** — 把 Java `CompletableFuture` 变成纯 std 的 Rust `Future`（[rjava::future](https://docs.rs/rjava/latest/rjava/future/)）；把 Rust **async** 函数注册为返回 `CompletableFuture` 的 Java native（[macro.async_native](https://docs.rs/rjava/latest/rjava/macro.async_native.html)）；把 Java 监听器回调变成可 await 的 future（[rjava::rx](https://docs.rs/rjava/latest/rjava/rx/)，`interface` feature）；以及异步方法调用（[Java::call_async](https://docs.rs/rjava/latest/rjava/struct.Java.html#method.call_async)）——全部纯 std、与执行器无关
- **`run_main`** — 调用 Java 的 `main` 入口 — [Java::run_main](https://docs.rs/rjava/latest/rjava/struct.Java.html#method.run_main)
- **数组** — `JArray<T>`、`Vec<T>` ⇄ `int[]` / `String[]` / `Object[]`、可空对象数组 — [struct.JArray](https://docs.rs/rjava/latest/rjava/struct.JArray.html)

### 快速开始

```rust,no_run
use rjava::prelude::*;

fn main() -> JavaResult<()> {
    let java = Java::builder().option("-Xmx256m").build()?; // 创建 JVM
    let sb = java.new_object("java.lang.StringBuilder", ("你好",))?;
    sb.call_void("append", ("，世界",))?;
    let s: String = sb.call("toString", ())?;
    println!("{s}");
    Ok(())
}
```

需要本机已安装 JDK（设置 `JAVA_HOME`）。完整示例与全部 API 见
[docs.rs](https://docs.rs/rjava/latest/rjava/)。要点：元组即参数列表；
返回类型由调用处标注决定；Java 异常转为 `JavaError::JavaException` 且
已在返回前捕获清除；`JavaError` 实现 `Display`（`{e}` 打印与 Java 异常
一致），动态调用失败自动带上操作上下文（`JavaError::WithContext` /
`with_operation`，如 `calling parseInt(String) on java.lang.Integer`）；
`Option<T>` 接受 `null`。

### bind! 类绑定

动态路径（`java.new_object("java.lang.StringBuilder", ("你好",))?` +
`sb.call("length", ())?`）靠字符串、运行期检查。对常用类，`bind!`
一次性声明类与方法的 Rust 类型，之后每次调用都是带类型的直接方法：

```rust,no_run
use rjava::bind;
use rjava::prelude::*;

bind! {
    "java.lang.StringBuilder" => StringBuilder {
        fn append(s: &str) -> Self;   // -> Self 重新包装：可链式调用
        fn length() -> i32;
        fn toString() -> String;
    }
}

fn demo(java: &Java) -> JavaResult<()> {
    let sb = java.new::<StringBuilder>(("你好",))?; // 构造参数仍是元组
    sb.append("，世界")?;
    let len: i32 = sb.length()?;
    Ok(())
}
```

- 构造：`java.new::<T>(args)`（走构造器）；包装已有句柄用
  `java.wrap::<T>(obj)`（不发 JNI 调用，首次真实调用时才惰性解析类）。
- `static fn` 生成的方法第一个参数是 `&Java`：`Math::max(java, 3, 7)?`。
- 动态路径仍然可用：`JavaBound::obj` 借用底层 `JObject`，类型化调用与
  动态调用可混用；`into_obj` 把包装器还原为句柄。
- 类引用按包装类型缓存一次（`OnceLock` + 全局引用，无 `unsafe`）；方法
  ID 每次调用重新派生——jni-rs 的 `MethodID` 生命周期使得缓存它不安全。
- 字段 — `field name: Type;` 把 Java 字段绑定成类型化访问对：
  `field label: String;` 生成 `get_label()` / `set_label(v)`（读写 Java 字段
  `label`；`static field MAGIC: i32;` 生成 `get_static_magic(java)` /
  `set_static_magic(java, v)`）。`bool` 字段的 getter 在字段不存在时回退到
  bean 风格访问方法 `get<Name>()` / `is<Name>()`。
- Java 名别名 — `fn to_string() -> String [java_name = "toString"];` 调用
  Java 方法 `toString`，Rust 方法名保持地道的 `to_string`（只改变 JNI 调用
  目标；`interface!` 同样支持）。

完整参考：[macro.bind](https://docs.rs/rjava/latest/rjava/macro.bind.html)
· [rjava::bind](https://docs.rs/rjava/latest/rjava/bind/)。

### interface — 从 Rust 实现 Java 接口

可选 `interface` feature（默认关闭）是 mlua `create_function` 在 Java 侧的
对应物：**从 Rust 实现 Java 接口，零手写 Java 实现类。** JDK 自带的
`java.lang.reflect.Proxy` 在运行期生成接口实现；rjava 只附带一个固定预编译
的 shell 类（一个 `InvocationHandler`，`javac --release 8` 预编译、类文件
版本 52，**已提交**在 `interface/java/rjava/shell/InvocationHandlerShell.class`
并经 `include_bytes!` 嵌入——构建完全不需要 `javac`）。首次使用时每进程从
临时目录经 `URLClassLoader` 引导一次——运行期不做任何生成或编译。

```rust,no_run
use rjava::interface;
use rjava::jni::JValueOwned;
use rjava::prelude::*;
use std::sync::Arc;

// Java 侧——接口就是普通 Java，没有任何 rjava 特有内容：
//   public interface Greeter {
//     String greet(String name);
//     int add(int a, int b);
//     long add(long a, long b);   // add(int,int) 的重载
//   }

fn make_greeter(java: &Java) -> JavaResult<JObject> {
    // Rust 侧——一个闭包实现全部方法。每次调用以
    // `interface::Call { name, param_types, args }` 送达：按方法名分发，
    // 重载按声明的参数类型区分。`Arc<dyn Handler>` 参数让闭包签名自动
    // 推断：无需生命周期标注。
    interface::proxy(
        Arc::new(|env: &mut jni::Env, call: interface::Call| {
            let mut args = call.args.into_iter();
            match call.name.as_str() {
                "greet" => {
                    let who = String::from_java(env, args.next().expect("greet takes 1 arg"))?;
                    Ok(interface::box_value(env, JValueOwned::Object(env.new_string(format!("Hello, {who}"))?.into()))?)
                }
                "add" if call.param_types == ["int", "int"] => {
                    let a = i32::from_java(env, args.next().expect("add(int,int) takes 2 args"))?;
                    let b = i32::from_java(env, args.next().expect("add(int,int) takes 2 args"))?;
                    Ok(interface::box_value(env, JValueOwned::Int(a + b))?)
                }
                "add" if call.param_types == ["long", "long"] => {
                    let a = i64::from_java(env, args.next().expect("add(long,long) takes 2 args"))?;
                    let b = i64::from_java(env, args.next().expect("add(long,long) takes 2 args"))?;
                    Ok(interface::box_value(env, JValueOwned::Long(a + b))?)
                }
                other => Err(JavaError::InvalidArgument("unexpected method")),
            }
        }),
        &["com.example.Greeter"], // 接口的二进制名称
    )
}
```

- `interface::proxy(Arc<Handler>, &[接口二进制名])` — 返回 JDK 生成的代理
  对象；`Arc<dyn Handler>` 参数让闭包签名自动推断（无需生命周期标注）。
- Handler — `dyn Fn(&mut Env, interface::Call) -> JavaResult<JObject>
  + Send + Sync + 'static`，其中 `Call { name: String, param_types: Vec<String>, args: Vec<JValueOwned> }`：
  用 `match call.name.as_str()` 分发，重载用 `call.param_types` 区分
  （`["int","int"]` 与 `["long","long"]`）。基本类型参数已拆箱
  （`int` → `JValueOwned::Int`，…），用现有 `FromJava` 转换。
- `Object` 方法被拦截 — `toString()`、`hashCode()`、`equals(Object)`
  永不到达 handler；库以 JDK 身份默认语义应答（标准
  `ClassName@hexhash` 格式串、`System.identityHashCode` 装箱、引用同一性）。
- 精确签名匹配 — 拦截按完整声明签名而非仅方法名：仅同名的领域方法
  （如 `toString(int)`）仍会到达 handler。这三个方法在 v1 中为库保留。
- `default` 方法自动转发 — 代理的 `default` 方法经反射调用
  `InvocationHandler.invokeDefault` 转发到其 Java 默认实现（Java 16+；
  shell 类仍以 `--release 8` 编译）。Java < 16 时回退到 handler。
- `box_value` / `null` — 构造返回值：`box_value` 把基本类型结果装箱成
  包装类（代理生成代码会再拆箱），`void` 方法返回 `null`。
- 状态与错误 — 闭包捕获的状态存于 userdata 注册表（代理不可达时自动
  释放）；handler 错误变成抛出的 Java 异常（panic → `RuntimeException`）。
- 无需构建要求 — 唯一的 shell 类已预编译并提交，启用该 feature 构建时
  **不需要 `javac`**；新鲜度守卫测试保持 `.java` 与提交的 `.class` 同步
  （`javac` 缺失时响亮地跳过）。

### interface! 类型化接口

上面的闭包 handler 是**动态**路径。`interface!` 是**类型化**路径——`bind!`
在"实现侧"的镜像：用 Rust 类型声明一次接口，宏生成 Rust `trait`，为你的状态
结构体实现它，`interface::proxy_typed` 在幕后构造通用 handler：

```rust,no_run
use rjava::interface;
use rjava::prelude::*;
use std::sync::Arc;

rjava::interface! {
    "com.example.Greeter" => Greeter {      // 生成：pub trait Greeter
        fn greet(name: String) -> String;
        fn add(a: i32, b: i32) -> i32;
        fn ping();                          // void 语法糖 -> JavaResult<()>
    }
}

struct MyGreeter { prefix: String }

impl Greeter for MyGreeter {
    fn greet(&self, _env: &mut jni::Env, name: String) -> JavaResult<String> {
        Ok(format!("{}{name}", self.prefix))
    }
    fn add(&self, _env: &mut jni::Env, a: i32, b: i32) -> JavaResult<i32> {
        Ok(a + b)
    }
    fn ping(&self, _env: &mut jni::Env) -> JavaResult<()> { Ok(()) }
}

fn make_greeter() -> JavaResult<JObject> {
    interface::proxy_typed::<dyn Greeter + Send + Sync>(
        Arc::new(MyGreeter { prefix: "Hi, ".into() }),
        &["com.example.Greeter"],           // 接口二进制名
    )
}
```

- 生成的每个 trait 方法都接收 `&self`、`jni::Env`、声明的参数并返回
  `JavaResult<R>`——参数用 `FromJava`、返回值用 `ToJava` 自动转换，无需手写
  `JValueOwned`；`fn ping();` 是 `void` 语法糖（`JavaResult<()>`）。
- 重载按 `(name, param_types)` 对声明的方法表分发，与动态路径一致；
  `[java_name = "…"]` 别名在这里同样可用
  （`fn add_long(a: i64, b: i64) -> i64 [java_name = "add"];`）。
- 三个库保留的 `Object` 方法（`toString()`、`hashCode()`、`equals(Object)`）
  在 `interface!` 中**编译期拒绝**——它们在到达 handler 前就被拦截，声明这种
  精确签名的方法永远不会被调用。
- `proxy_typed::<dyn Trait + Send + Sync>(Arc::new(state), &[接口名])`——状态
  在多次调用间共享（`Send + Sync + 'static`），一个状态结构体可实现多个接口
  （再写一个 `interface!` 生成下一个 trait）。完全动态的场景（`static` 接口
  方法、任意分发）仍使用通用 `proxy(Arc<Handler>, …)`。

完整参考：[rjava::interface](https://docs.rs/rjava/latest/rjava/interface/) ·
[macro.interface](https://docs.rs/rjava/latest/rjava/macro.interface.html)。

### serde ↔ Map 与 JavaBean

`serde` feature（选用，与 `interface` 一样）把 Rust 结构体映射到两类 Java
契约：`java.util.HashMap` / `ArrayList` 值树（`JavaMap<T>` /
`rjava::serde::from_object`），以及经 getter/setter 反射的普通 Java Bean
（`JavaBean<T>`）。完整参考：[rjava::serde](https://docs.rs/rjava/latest/rjava/serde/) ·
[rjava::bean](https://docs.rs/rjava/latest/rjava/bean/)。

- **Java record** — 读取走组件访问器 `x()`（先试 `get<Name>` / `is<Name>`；
  普通 bean 不会被探测 `x()`）；写入走**规范构造器**（record 没有 setter）。
  Rust 结构体的字段顺序必须与 Java 组件顺序一致——运行期检查，不一致时报错
  并指出两个顺序。record 映射以 **Java 16 为 JVM 下限**（`Class.isRecord()`）；
  更老的 JVM 把该类当普通 bean 处理。
- **bean 嵌套 bean** — 类型为 `JavaBean<Inner>` 的结构体字段序列化为嵌套 bean
  对象（`new` + setter 构造，record 用规范构造器）传给属性的 setter；写入侧
  用 `__rjava_bean_class` 标记携带类名，读取侧从嵌套对象的运行期类推导。经
  `JavaMap` 值树时同一字段落成普通嵌套 `HashMap`，可正常往返。
- **`char` 支持** — 值层面 `char` ⇄ `java.lang.Character`（超出 `U+FFFF` 的
  码点报错），并补齐精确的 `Byte` / `Short` / `Float` 包装（`i8`/`u8` →
  `Byte`、`i16` → `Short`、`f32` → `Float`，不做拓宽）。`u8` 以 `byte` 的
  无符号解读读回（`byte` `-1` → `u8` `255`），因此每个 `u8` 都能往返；
  `Vec<Vec<T>>` 以嵌套 `ArrayList` 双向流动。

### 异步：Future、native 与回调

四条路径覆盖 `CompletableFuture` 与 Rust `Future` 的双向桥接，全部**纯 std**
（无执行器依赖、无 `unsafe`）：

1. **`java_future`** — 把 Java `java.util.concurrent.CompletableFuture`
   变成 Rust `Future`：首次 poll 派一个脱离 std 线程阻塞在 `get()` 上，用
   `FromJava` 转换值并唤醒 future — [rjava::future](https://docs.rs/rjava/latest/rjava/future/)。
2. **`async_native!`** — 反向：把 Rust **async** 函数（函数项或闭包）注册为
   返回 `CompletableFuture<R>` 的 Java 静态 native；每次调用在独立 std 线程
   上跑完 Rust future 再完成 Java future — [macro.async_native](https://docs.rs/rjava/latest/rjava/macro.async_native.html)。
3. **`rx::from_callback`**（`interface` feature）— Java 监听器回调变成可
   await 的**一次性** Rust future：把接口名和一个构造普通 `interface::proxy`
   handler 的闭包交给 `from_callback`（不需要 `interface!` /
   `proxy_typed`），把返回的代理注册到 Java 宿主，future 在第一个事件到达时
   完成 — [rjava::rx](https://docs.rs/rjava/latest/rjava/rx/)。
4. **`Java::call_async` / `Java::call_static_async`** — `call` /
   `call_static` 的异步版：JVM 调用在 worker 线程上执行，结果经 future 返回 —
   [Java::call_async](https://docs.rs/rjava/latest/rjava/struct.Java.html#method.call_async)。

完整闭环 —— Rust → Java → Rust：

```rust,no_run
use std::sync::Arc;
use rjava::async_native;
use rjava::future::java_future;
use rjava::interface::{self, Call};
use rjava::prelude::*;
use rjava::rx;

// 1) Rust async 函数注册为 Java native `AsyncNativeDemo.compute`——
//    Java 侧看到的是 `CompletableFuture<Integer>`
async fn compute(_java: Java, (a, b): (i32, i32)) -> JavaResult<i32> {
    Ok(a + b)
}

# async fn demo(java: &Java) -> JavaResult<()> {
let clazz = java.class("AsyncNativeDemo")?;
clazz.register_natives(&[async_native!("compute", compute)?])?;

// 2) 从 Rust await CompletableFuture：Rust -> Java -> Rust
let cf: JObject = clazz.call_static("compute", (1_i32, 2_i32))?;
let sum: i32 = java_future(java.clone(), cf).await?;
# let _ = sum;

// 3) Java 监听器回调变成可 await 的 future（`interface` feature）
let (listener, event) = rx::from_callback::<String, _>(&["Listener"], |tx| {
    Arc::new(move |env, call: Call| {
        if call.name == "onEvent" {
            let value =
                String::from_java(env, call.args.into_iter().next().expect("onEvent takes 1 arg"))?;
            let _ = tx.send(Ok(value));
        }
        Ok(interface::null())
    })
})?;
let host: JObject = java.new_object("Host", (&listener,))?;
host.call_void("fire", ("hello from Java",))?;
let value: String = event.await?;
# let _ = value;

// 4) 异步方法调用：tokio 运行时存在时走阻塞线程池
let sb: JObject = java.new_object("java.lang.StringBuilder", ())?;
let s: String = java.call_async(&sb, "toString", ()).await?;
# let _ = s;
# Ok(())
# }
```

语义要点：

- **每次调用一个线程，无执行器。** `java_future` 在一个脱离 std 线程上阻塞
  `get()`；每次 `async_native!` 调用在自己的 std 线程上用
  `futures_lite::future::block_on` 跑完 Rust future——真实的线程 park
  waker，因此与执行器无关的 future 都能完成（纯计算、其它 rjava 桥、在
  `poll` 中 park 等待唤醒的 future、跨线程通道）。启用 `tokio` feature
  后，需要 tokio 运行时的 future（定时器、IO、tokio 通道）在 native 于
  tokio 运行时内被调用时可以运行：future 会被 spawn 到该运行时上，否则
  回退到 std 线程。`call_async` 同理：每次调用一个 worker 线程。
- **`tokio` feature（可选，默认关闭）。** 启用 `features = ["tokio"]` 后，
  `call_async` / `call_static_async` 在当前存在 tokio 运行时的情况下经
  `tokio::task::spawn_blocking` 派发 JVM 调用；没有运行时（或未启用该
  feature）则回退到脱离 std 线程——启用它绝不会影响非 tokio 用户，纯 std
  路径也永远不要求 tokio。
- **错误。** `async_native!` 的 `Err(JavaError::JavaException { class, message })`
  会实例化为 `class` 的异常并经 `completeExceptionally` 送达；其它 `Err`
  与 panic 则以 `java.lang.RuntimeException` 异常完成（与同步 native 的
  异常规则一致）。`java_future` await 这样的 future 时经通常的
  `ExecutionException` cause 解包上报。
- **注册冲突。** 两个 Rust 参数元组类型相同的不同 `async_native!` 注册会共
  用一个跳板（只按 `A` 键控——Java 侧返回的永远是 `CompletableFuture`），
  `register_natives` 拒绝第二个，与同步路径一致。v1 中 **async native 没有
  显式签名形式**——请给两个方法不同的 Rust 参数元组类型。
- **取消。** 丢弃 Rust future 从不取消 Java 侧：`async_native!` 的 worker
  会继续跑（Java 侧 `cancel()` 在 v1 不传播）、被丢弃的 `call_async`
  future 会丢弃在途调用的结果、`java_future` 的取消是 Java 侧的事
  （`cf.call_void("cancel", (true,))`）。
- **一次性回调。** `rx::from_callback` 以第一个事件完成，之后的 send 被
  丢弃。丢弃 future 不会注销监听器——首个 poll 之前丢弃不派生任何 worker
  （之后 handler 的 `send` 会失败，可观察）；首个 poll 之后丢弃则 worker
  继续等待；需要时从 Java 侧取消（移除监听器）。

### 功能说明与限制

- 没有无符号整数：Java 无 `u32`/`u64`（也无 `u16`），用 `char` 对应
  `char`、`Vec<u8>` 对应 `byte[]`。
- 拆箱要求精确的包装类型：不做拓宽（`Integer` 不匹配 `long` 参数）、
  `Number` 不拆箱、`null` 永不匹配基本类型参数。
- 参数上限：类型推导形式最多 64 个元组元素（实例 native 为 63）；显式
  签名形式可达 JVM 的 255 参数单位上限（JVMS §4.3.3）。
- 依赖名必须是 `rjava`：宏展开引用 `::rjava::…` 路径。
- JVM 复用：`Java::builder().build()` 复用本库已创建的 JVM；Rust 嵌入
  Java 进程时，在 native 方法里用 `Java::from_env`（传入
  `&mut jni::Env`）接入正在运行的 JVM。
- Java 源码不允许 `native` 构造器（JLS）：用静态工厂或"直接 new"（普通
  构造器调用 native `init()` 绑定状态）。
- 插件模型 = API jar：不做运行期 `.class` 生成；API jar 是插件开发者
  面对的编译期契约。
- 同签名冲突规则：两个不同、同签名的类型推导 native 会共用一个跳板，
  注册时被拒绝并给出可操作的错误；显式签名形式是逃生门。
- userdata 自动释放：绑定持有弱全局引用，对象不被钉住；后台清理线程
  （约 500 ms）+ 惰性清理会自动释放死绑定及其 Rust 状态。
- `serde` feature 是选用的（`features = ["serde"]`）。
- `interface` feature 是选用的（`features = ["interface"]`）。唯一的 shell
  类已预编译（`javac --release 8`）并**提交**在
  `interface/java/rjava/shell/InvocationHandlerShell.class`，经
  `include_bytes!` 嵌入——启用该 feature 构建时**不需要 `javac`**（构建期
  完全不需要 JDK）；新鲜度守卫测试保持 `.java`/`.class` 同步，`javac`
  缺失时响亮地跳过。运行期每进程从临时目录经 `URLClassLoader` 引导
  一次——不做任何运行期生成。
- `tokio` feature 是选用的（`features = ["tokio"]`）：只影响
  `call_async` / `call_static_async` 的派发——当前存在 tokio 运行时走
  `tokio::task::spawn_blocking`，否则回退 std 线程；启用它不会影响非
  tokio 用户（见「异步：Future、native 与回调」）。
