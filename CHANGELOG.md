# Changelog

All notable changes to this project are documented in this file.

The format is loosely based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Following the standard convention for 0.x releases: 0.x means pre-1.0, so
breaking changes may arrive in any minor or patch release until 1.0.

## [0.1.1] - 2026-08-10

### Added
- `register_natives!` — the batch form of `JClass::register_natives`:
  `register_natives!(clazz, native!("a", a), native_inst!("b", b), async_native!("c", c))?`
  registers several native methods with **one** `?` and no array brackets
  (each item's `JavaResult` unwraps inside the generated array). Accepts any
  mix of `native!` / `native_inst!` / `async_native!` items and an optional
  trailing comma; at least one method is required. Implemented as a
  declarative macro in the crate itself — no new dependency, rjava-macros
  untouched (its 0.1.0 stays live).

## [0.1.0] - 2026-08-09

First public release. `rjava` is a safe, ergonomic Java-interop crate for
Rust, modeled after `mlua`: a small wrapper over the JNI with no `unsafe` in
its own source (`#![forbid(unsafe_code)]`). Everything below is implemented
and tested against a real JVM (JDK 21).

Rename: the proc-macro crate is now rjava-macros, the unsafe helper rjava-helper (rjava-macros-proc retired).

### Core API
- `Java::builder()` — create a JVM via the invocation API (class path,
  raw `-X…` options); `build()` reuses a JVM already created through this
  crate, and embedded-in-Java users attach with `Java::from_env` from a
  native method's `&mut jni::Env`.
- Class/object handles: `java.class("…")`, `new_object`, `JClass`, `JObject`,
  runtime class lookup, `null` handling.
- Method calls: `call`, `call_void`, `call_static`, `get_field` /
  `set_field`, `get_static_field` / `set_static_field` — Rust tuples as
  argument lists, `JavaResult<T>` for error propagation.
- Java-side constructor binding: Java `new X()` yields a bound Rust object.

### Ergonomics & parameter limits
- **Type-derived form** (primary): `native!("add", add)` — the JNI signature
  is derived from the Rust types at runtime; **no signature strings**.
- **Explicit-signature form** (escape hatch): `native!("add", "(II)I", add)`
  — the descriptor is parsed at compile time by a hand-written parser; a
  malformed signature is a compile error with a helpful message.
- String signatures, autoboxing in the reflection fallback (with the
  no-widening rule), and an array facade (`JArray<T>`, `Vec<T>` ⇄ `T[]`,
  `Vec<T>` object arrays).
- Type-derived form: argument tuples up to **64 elements** (63 declared
  parameters for instance methods — the receiver takes the 64th slot).
- Explicit form: any signature within the JVM's own method-descriptor limit
  of 255 parameter units (JVMS §4.3.3; `long`/`double` count as two, an
  instance method's `this` consumes one).

### Native methods
- Register Rust functions as Java `native` methods via `JClass::register_natives`
  and the `native!` / `native_inst!` macros (`native_inst!` receives `this`).
- Fn items **and** closures (`Send + Sync + 'static`) are supported.
- Panics are caught and converted into Java `RuntimeException`s; `Err(…)`
  results are thrown as Java exceptions — nothing unwinds across the FFI
  boundary.

### userdata
- Rust-backed Java objects (the `mlua::UserData` analog): `bind` / `get` /
  `unbind` / `create_shell` — a Rust `T: Send + Sync + 'static` is stored
  behind an `Arc` on a Java sidecar object.
- **Auto-release on GC**: bindings hold JNI **weak global references**, so
  the shell is never pinned; when the Java sidecar is garbage-collected,
  the binding (and its Rust state) is released automatically by a
  background **cleaner thread** (~500 ms tick) plus **lazy draining** on
  every userdata call.

- **`userdata::with::<T, R>(env, obj, |state| …)`** — closure sugar over
  `userdata::get`: fetch the bound state, borrow it as `&T` for the closure,
  return its result — `with(env, &this, |c| c.value())` instead of
  `Ok(get(env, &this)?.value())`, one line per native method. Missing or
  wrongly-typed state fails with exactly the same `InvalidArgument` as
  `get`.

### Plugin workflow
- **API jar as the compile-time contract**: the API jar — the interfaces
  plus a `Bridge` class declaring `native` methods — is what plugin
  developers compile against; there is **no runtime `.class` generation**.
- Runtime loading over `java.net.URLClassLoader`: `Java::load_jar` and
  `Java::class_loader(_with_parent)` (parent delegation verified
  empirically).
- `register_natives` injects the Rust implementations into the `Bridge`;
  plugin jars load with the API loader as their parent, so plugin code
  resolves the API interfaces and the Rust-backed `Bridge` through it.

### serde ↔ Map
- Opt-in `serde` feature: Rust structs ⇄ `java.util.HashMap` /
  `java.util.ArrayList` value trees via the `JavaMap` wrapper and
  `rjava::serde::from_object`.
- Type mapping: primitives → boxed wrappers, `String`, `Option<T>` ⇄ `null`,
  `Vec<T>`/tuples ⇄ `ArrayList`, nested structs ⇄ nested `HashMap`.
- Serialization errors on `u16`/`u32`/`u64`, `usize`/`isize`,
  `i128`/`u128`, and enums (the no-unsigned-integers rule carries over).
- **`char` supported** — `char` ⇄ `java.lang.Character` (code points above
  `U+FFFF` error), with exact `Byte` / `Short` / `Float` wrapper mapping
  (`i8`/`u8` → `Byte`, `i16` → `Short`, `f32` → `Float` — no widening).
- **`u8` read fix** — a `Byte` value reads back with the unsigned
  interpretation of the byte (`byte` `-1` → `u8` `255`), so every `u8`
  round-trips instead of erroring on high values.
- `Vec<Vec<T>>` ⇄ nested `ArrayList`s verified in both directions.

### Bean mapping
- `JavaBean` (feature `serde`): Rust structs ⇄ plain Java beans via
  getter/setter reflection — primitives, `String`, `Option` (→ `null`),
  `Vec<T>`/arrays/tuples ⇄ `ArrayList`, nested structs ⇄ nested `HashMap`,
  with the no-widening unboxing rule.
- **Java records** — a record is read through its component accessor `x()`
  (after `get<Name>` / `is<Name>`; plain beans are never probed for `x()`,
  so an unrelated no-argument method can't collide) and written through its
  **canonical constructor** (records have no setters) with the struct's
  field values in declaration order. The canonical parameter order is the
  component order, so the Rust field order must match — checked at runtime
  against the record's component names and errors loudly naming both orders
  when they differ. Detection is a static `Class.isRecord()` call, so
  records document **Java 16 as the JVM floor**; a pre-16 class cannot be a
  record and is treated as a plain bean.
- **Bean-to-bean nesting** — a struct field typed `JavaBean<Inner>`
  serializes to a nested bean object (`new` + setters, or the canonical
  constructor for a record; nested beans recurse) via the reserved
  `__rjava_bean_class` marker, passed to the outer property's setter;
  reads derive the class from the nested object's runtime class, and the
  marker map from the `JavaMap` value tree round-trips too (the marker
  reads back as the plain struct).

### bind! — compile-time-typed class bindings
- `rjava::bind!` declares a class binding once — class name, wrapper name,
  and methods with their Rust types; the JNI descriptors are computed at
  compile time (no signature strings, mirroring the `ToJava`/`FromJava`
  type mapping).
- Instance methods are typed calls (`fn append(s: &str) -> Self`);
  `-> Self` re-wraps the returned object so calls chain. `static fn`
  declarations generate a method that takes `&Java` first
  (`Math::max(&java, 3, 7)?`).
- Construction: `Java::new::<T>(args)` (constructor) and
  `Java::wrap::<T>(obj)` (existing handle — no JNI call; the class resolves
  lazily on the first real call). The dynamic path stays available via
  `JavaBound::obj` / `into_obj`.
- The class reference is cached once per wrapper (`OnceLock` over a JNI
  global reference — no `unsafe`); **method IDs are re-derived per call by
  design**, since jni-rs's `MethodID` lifetime tied to the `Env` makes
  caching them unsafe in an `unsafe`-free crate.
- **`field` declarations** — `field name: Type;` binds a Java field as a
  typed accessor pair: `field label: String;` generates
  `get_label(&self) -> JavaResult<String>` / `set_label(&self, v)` over the
  Java field named `label`; `static field MAGIC: i32;` generates
  `get_static_magic(java: &Java)` / `set_static_magic(java, v)`. A `bool`
  field's getter tries the raw field first and falls back to the bean-style
  accessor methods `get<Name>()` then `is<Name>()` when the field does not
  exist (the `bean` module's read order).
- **`[java_name = "…"]` aliases** — `fn to_string() -> String
  [java_name = "toString"];` targets the Java method `toString` while the
  Rust method keeps the idiomatic name (the JNI call is the only thing that
  changes); empty and duplicate Java names are compile errors.

### interface — Java interfaces from Rust
- Opt-in `interface` feature (off by default, like `serde`): implement
  Java interfaces from Rust with **zero user-written Java classes** — the
  JDK's `java.lang.reflect.Proxy` generates the implementation at runtime,
  and rjava ships exactly one fixed precompiled shell class
  (`rjava.shell.InvocationHandlerShell`), **precompiled** (`javac
  --release 8`, class-file version 52) and **committed** at
  `interface/java/rjava/shell/InvocationHandlerShell.class`, embedded in
  the library via `include_bytes!` — no build environment needs `javac`
  (a freshness guard test recompiles the `.java` and fails if the
  committed `.class` goes stale, skipping loudly when `javac` is absent).
- **docs.rs build fix** (88c6f3b): docs.rs builds with `--all-features`
  failed because the `interface` feature's `build.rs` needed `javac` to
  compile the shell class; fixed by precompiling the fixed shell class and
  committing it — docs.rs and all consumers now build javac-free.
- Bootstrap: on first use the embedded `.class` is written to a
  per-process temp dir and loaded through a `URLClassLoader` — once per
  process, cached; nothing is generated or compiled at runtime.
- `interface::proxy(Arc<Handler>, &[iface_names]) -> JavaResult<JObject>`:
  one closure (`Handler = dyn Fn(&mut Env, interface::Call) ->
  JavaResult<JObject> + Send + Sync + 'static`) implements every interface
  method; each call arrives as `interface::Call { name, param_types, args }`
  — dispatch on `call.name.as_str()` and distinguish overloads via
  `call.param_types` (`["int","int"]` vs `["long","long"]`); primitive
  arguments arrive unboxed and convert with the existing `FromJava` impls.
- `interface` hardened — the three `Object` methods `toString()` /
  `hashCode()` / `equals(Object)` are intercepted library-side with the
  JDK's identity defaults (standard `ClassName@hexhash` format string,
  `System.identityHashCode` boxed, reference identity), never reaching the
  handler; interception matches the exact signature, not the name alone,
  so same-named domain methods (`toString(int)`, …) still reach the
  handler (library-reserved in v1). Default interface methods auto-forward
  to their Java default implementation via reflective
  `InvocationHandler.invokeDefault` (Java 16+; the shell class stays
  compiled with `--release 8`); on Java < 16 they fall back to the handler.
- Helpers: `interface::box_value` (build the returned handle; boxes
  primitive results into their wrapper classes, unboxed again by the
  proxy's generated code) and `interface::null` (the `void`-method
  return). Handler errors become thrown Java exceptions; panics →
  `RuntimeException`; captured state lives in the userdata registry and is
  released automatically when the proxy becomes unreachable.
- **`rjava::interface!`** — the typed mirror of `bind!` on the implement
  side: declare the interface once with Rust types; the macro generates a
  `pub trait` (one method per declared method, each taking `&self`, the
  `jni::Env`, the declared parameter types, and returning `JavaResult<R>` —
  `fn ping();` is the `void` sugar for `JavaResult<()>`), and
  `interface::proxy_typed::<dyn Trait + Send + Sync>(Arc::new(state),
  &[iface_names])` builds the generic handler behind the scenes — no
  `JValueOwned` handling, no name-match dispatch. Arguments convert with the
  declared types' `FromJava` impls (no widening — a declared `i32` receives
  the unboxed `int`, never a widened `Long`), returns with `ToJava` +
  `box_value`.
- Dispatch matches `(name, param_types)` against the declared method table
  (overloads by declared parameter types, like the dynamic path);
  `[java_name = "…"]` aliases work here too. The three library-reserved
  `Object` methods (`toString()` / `hashCode()` / `equals(Object)`) are
  **compile-time rejected** in `interface!` — they are intercepted before
  the handler, so a trait method with that exact signature could never be
  called (same-named domain signatures like `toString(int)` still pass).
  `static fn` is not supported in v1 (static interface methods cannot be
  trait items) — the generic `proxy(Arc<Handler>, …)` path remains the
  dynamic escape hatch. A call matching no declared method errors clearly,
  naming the method and the arriving parameter types.

### Async bridge
- `java.util.concurrent.CompletableFuture` ⇄ Rust `Future`s — std-only, no
  executor; primitives are unboxed from the wrapper object, `null` with a
  primitive annotation is an error (annotate `Option<T>` to accept `null`).
- **`rjava::async_native!`** (std-only, no feature) — register a Rust
  **async** function (fn item or closure) as a Java static `native`
  returning `java.util.concurrent.CompletableFuture`: Java callers chain and
  `await` it; the Rust future runs thread-per-call on a detached std thread
  and completes the Java future (`complete` / `completeExceptionally`).
  `Err(JavaError::JavaException { class, message })` is materialized as an
  instance of `class`; other `Err`s and panics complete exceptionally with a
  `java.lang.RuntimeException`. Two registrations with the same Rust
  argument-tuple type share one trampoline and `register_natives` rejects
  the second (the explicit-signature form is **not** available for async
  natives in this version); Java-side `cancel()` does not propagate to the
  Rust future in v1.
- **`async_native!` waker fix** — replaced the internal noop-waker **spin**
  `block_on` with `futures_lite::future::block_on` (new non-optional
  dependency futures-lite **2.6.1**; real thread-parking waker), so
  executor-agnostic futures — including ones that park in `poll` waiting for
  a wakeup — now complete on the std path. `async_native!` is now
  **tokio-aware** too: with the `tokio` feature the future is spawned on the
  current tokio runtime when one is in scope (`Handle::try_current()`), so
  tokio timers and IO work; otherwise it falls back to the std thread.
- **`rjava::rx::from_callback`** (feature `interface`) — a Java listener
  callback becomes an awaitable **one-shot** Rust future:
  `from_callback(ifaces, |tx| handler) -> (proxy, impl Future)`; the plain
  `interface::proxy`-style closure handler sends the first event's value and
  the future resolves with it (std-only, executor-agnostic). Dropping the
  future does not unregister the listener; dropped before the first poll it
  spawns nothing (the handler's later `send`s fail observably).
- **`Java::call_async` / `Java::call_static_async`** (std-only) — the async
  analog of `call` / `call_static`: the JVM call runs on a worker thread
  (lazy start on first poll; fused; dropping the future discards the
  in-flight call's result). With the new `tokio` feature, a current tokio
  runtime dispatches the call via `tokio::task::spawn_blocking`; otherwise a
  detached std thread runs it.
- **`tokio` optional feature** (default off): `tokio = { version = "1",
  features = ["rt","time","sync"], optional = true }` wired as
  `tokio = ["dep:tokio"]`; it affects the `call_async` worker dispatch and
  makes `async_native!` spawn on a current tokio runtime (see the
  `async_native!` entry below), so enabling it never breaks non-tokio users.
- `examples/async_demo.rs` (required-features `["tokio","interface"]`) — the
  full async circle in one runnable demo: `java_future` over
  `CompletableFuture.supplyAsync` (Rust `Supplier` proxy), `call_async`
  StringBuilder round trip (tokio blocking pool), `rx::from_callback`
  listener fired from a Java thread, and `async_native!` registered and
  awaited back through `java_future`.

### run_main
- `Java::run_main` — invoke a Java `main` entry point from Rust.

### Error handling
- **`JavaError` now implements `Display`** — `println!("{e}")` renders like
  the Java exception itself (`java.lang.NumberFormatException: For input
  string: "x"`) instead of the derived `Debug` struct form; an empty message
  renders just the class. `Debug` is unchanged and stays the
  machine-readable rendering.

- **`JavaError::WithContext { source, operation }`** — dynamic-call failures
  now name the operation: `calling parseInt(String) on java.lang.Integer:
  java.lang.NumberFormatException: …` (methods render as
  `calling {name}({arg types}) on {class}`, constructors as
  `constructing {class}({arg types})`, fields as
  `reading/writing [static] field {name} on {class}`). Attached once by
  `JavaError::with_operation` (idempotent — a `WithContext` is never
  double-wrapped, so no stack of contexts); `Display` renders
  `{operation}: {source}` and the `Error::source` chain stays intact.

### API cleanup
- `Java::with_current` removed (pre-release — no deprecation): `build()`
  reuses a JVM already created through this crate, and embedded-in-Java
  users attach with `Java::from_env` from a native method's `&mut jni::Env`.
- `JClass::call_static_void` removed — use `call_static::<_, ()>`.
- `call_void` kept as a documented convenience: a thin wrapper over the
  void-returning call path, and the ergonomic primary form for void
  methods.

### Performance
- Benchmark suite (criterion, real JVM; not part of CI). Headline numbers
  from the benchmark-driven optimization round (JDK 21, Windows x64):
  - `Vec<i32>` ⇄ `int[]` conversions via bulk `get_region`/`set_region`
    instead of per-element JNI calls: the 1024-element round trip dropped
    **114 µs → 6.0 µs (write)** and **143 µs → 5.7 µs (read)**, ≈ −95%.
  - userdata identity lookup: `java/lang/System` cached as a process-global
    reference (`OnceLock<Global>`); `userdata::get` dropped **2.9 µs →
    1.6 µs** (−45%).
  - Native-call floor ≈ 1.0–1.5 µs; string conversions ≈ 0.3–0.7 µs.

### CI
- Cross-platform matrix (ubuntu / macos / windows), zero-warning gate
  (`-D warnings`), JDK 21 via Temurin, and a check that fails the job if any
  test silently skipped for lack of a JVM.
- CI hardened for the first GitHub Actions run: a dedicated `msrv` job
  (`cargo check --all-targets` on the exact 1.88.0 toolchain) guards the
  declared `rust-version = "1.88"`; the main job now clippys with
  `--all-features` (the serde-gated `serde`/`bean` modules are compiled and
  linted on CI), builds docs with `--all-features`, and compile-gates the
  criterion bench harness (`cargo bench --no-run`).
- Crates consolidated into a single Cargo workspace: shared `[workspace.package]` inheritance (version/license/repository/rust-version), one root `Cargo.lock`.

### Docs
- README rewritten as an introduction (badges, docs.rs pointers, slimmed
  中文 mirror); the exhaustive API documentation lives in the crate's
  rustdoc (docs.rs).
- README with quick start, module-level crate docs, a mlua↔rjava concept
  table, roadmap, and benchmark sections (EN + 中文).
- Examples: `examples/bind.rs` (typed `bind!` class bindings — chains, statics, fields, `[java_name]` aliases), `examples/interface.rs` (Rust-implemented Java interfaces via typed `interface!` + `proxy_typed` and dynamic `interface::proxy`, feature `interface`) and `examples/async_demo.rs` (the full async circle — `java_future`, `call_async`, `rx::from_callback`, `async_native!`; features `tokio` + `interface`).

## MSRV

Rust **1.88** — required by the crate's own code (let-chains, stabilized in
1.88) and by the dependency floors (jni 0.22 declares 1.85; clap 4.6.6, a
criterion dev-dependency, declares 1.85). Verified with
`cargo +1.88.0 check --all-targets` on all three crates.
