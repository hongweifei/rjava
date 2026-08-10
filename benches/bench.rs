#![forbid(unsafe_code)]
//! Criterion benchmarks for rjava's hot paths (see the crate README /
//! `docs` for the benchmark rationale).
//!
//! Every benchmark needs a real JVM: the harness compiles a small Java
//! fixture (`benches/java/BenchLib.java` plus the `Counter` userdata shell
//! from the integration tests) with `javac` from `JAVA_HOME` (or `PATH`),
//! then starts the JVM with `Java::builder()`. If no JVM (or no `javac`) can
//! be located, the harness prints a loud skip reason and exits without
//! producing numbers — it never silently benchmarks garbage.
//!
//! Benchmarks are not part of CI; they exist to keep the crate's per-call
//! overhead (native dispatch, userdata identity lookup, array conversions)
//! data-driven instead of vibes-driven.

use std::sync::OnceLock;

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use parking_lot::Mutex;
use rjava::jni::{JavaVM, JValueOwned};
use rjava::prelude::*;
use rjava::{native, native_inst};

// ---------------------------------------------------------------------------
// JVM + fixture bootstrap (mirrors tests/run_main.rs; benches are NOT CI)
// ---------------------------------------------------------------------------

/// Compile the bench fixtures exactly once per bench process. `Err` carries
/// the reason, reported once per group; when it fails every group skips.
static FIXTURE_COMPILED: OnceLock<Result<(), String>> = OnceLock::new();

fn compile_fixture() -> Result<(), String> {
    let out_dir = "target/rjava-bench-classes";
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
        .arg("benches/java/BenchLib.java")
        .arg("tests/java/com/example/Counter.java")
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

/// The one JVM for the bench process, plus its class path. `Err` carries the
/// skip reason; every group checks it and returns early with an `eprintln`.
static BENCH_JVM: OnceLock<Result<Java, String>> = OnceLock::new();

fn bench_jvm() -> &'static Result<Java, String> {
    BENCH_JVM.get_or_init(|| {
        if let Err(reason) = FIXTURE_COMPILED.get_or_init(compile_fixture) {
            return Err(format!("could not compile Java fixture: {reason}"));
        }
        Java::builder()
            .class_path("target/rjava-bench-classes")
            .build()
            .map_err(|e| format!("could not build the JVM: {e:?}"))
    })
}

/// Register the bench natives once per process (JNI `RegisterNatives` is
/// all-or-nothing per class, and re-registration would be redundant work).
static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();

fn register_natives() -> Result<(), String> {
    match REGISTERED.get() {
        Some(r) => r.clone(),
        None => {
            let result = (|| {
                let java = match bench_jvm() {
                    Ok(j) => j,
                    Err(reason) => return Err(reason.clone()),
                };
                let benchlib = java.class("rjbench.BenchLib").map_err(|e| format!("{e:?}"))?;
                // `range`/`sum`/`strings`/`totalLen` are all 1-arity
                // type-derived natives whose trampolines can fold to the same
                // address under the optimiser (same C ABI) — the shared-
                // trampoline duplicate check would then false-positive. The
                // explicit-signature form generates a unique trampoline per
                // registration, which is exactly what the bench needs.
                benchlib
                    .register_natives(&[
                        native!("nop", bench_nop).map_err(|e| format!("{e:?}"))?,
                        native!("add", bench_add).map_err(|e| format!("{e:?}"))?,
                        native!("range", "(I)[I", bench_range).map_err(|e| format!("{e:?}"))?,
                        native!("sum", "([I)J", bench_sum).map_err(|e| format!("{e:?}"))?,
                        native!("strings", "(I)[Ljava/lang/String;", bench_strings)
                            .map_err(|e| format!("{e:?}"))?,
                        native!("totalLen", "([Ljava/lang/String;)J", bench_total_len)
                            .map_err(|e| format!("{e:?}"))?,
                    ])
                    .map_err(|e| format!("registering BenchLib natives: {e:?}"))?;

                let counter = java
                    .class("com.example.Counter")
                    .map_err(|e| format!("{e:?}"))?;
                counter
                    .register_natives(&[
                        native!("create", bench_counter_create).map_err(|e| format!("{e:?}"))?,
                        native_inst!("increment", bench_counter_increment)
                            .map_err(|e| format!("{e:?}"))?,
                    ])
                    .map_err(|e| format!("registering Counter natives: {e:?}"))?;
                Ok(())
            })();
            let _ = REGISTERED.set(result.clone());
            result
        }
    }
}

/// Run `f` with the current thread attached (see tests/integration.rs).
fn with_env<T>(f: impl FnOnce(&mut rjava::jni::Env<'_>) -> JavaResult<T>) -> JavaResult<T> {
    let vm = JavaVM::singleton().map_err(JavaError::from)?;
    vm.attach_current_thread(f)
}

// ---------------------------------------------------------------------------
// Native fixtures
// ---------------------------------------------------------------------------

fn bench_nop(_env: &mut rjava::jni::Env, (): ()) -> JavaResult<()> {
    Ok(())
}

fn bench_add(_env: &mut rjava::jni::Env, (a, b): (i32, i32)) -> JavaResult<i32> {
    Ok(a + b)
}

fn bench_range(_env: &mut rjava::jni::Env, (n,): (i32,)) -> JavaResult<Vec<i32>> {
    Ok((0..n).collect())
}

fn bench_sum(_env: &mut rjava::jni::Env, (xs,): (Vec<i32>,)) -> JavaResult<i64> {
    Ok(xs.iter().map(|&x| x as i64).sum())
}

fn bench_strings(_env: &mut rjava::jni::Env, (n,): (i32,)) -> JavaResult<Vec<String>> {
    Ok((0..n).map(|i| format!("s{i}")).collect())
}

fn bench_total_len(_env: &mut rjava::jni::Env, (xs,): (Vec<String>,)) -> JavaResult<i64> {
    Ok(xs.iter().map(|s| s.len() as i64).sum())
}

/// The Rust state behind a `com.example.Counter` shell (mirrors the
/// integration-test fixture).
struct Counter(Mutex<i64>);

fn bench_counter_create(env: &mut rjava::jni::Env, (): ()) -> JavaResult<JObject> {
    rjava::userdata::create_shell(env, "com.example.Counter", Counter(Mutex::new(0)))
}

fn bench_counter_increment(
    env: &mut rjava::jni::Env,
    (this, by): (JObject, i64),
) -> JavaResult<i64> {
    let counter = rjava::userdata::get::<Counter>(env, &this)?;
    let mut count = counter.0.lock();
    *count += by;
    Ok(*count)
}

// ---------------------------------------------------------------------------
// Groups
// ---------------------------------------------------------------------------

fn bench_native_call(c: &mut Criterion) {
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING native_call benchmarks: {reason}");
        return;
    }
    let java = match bench_jvm() {
        Ok(j) => j.clone(),
        Err(reason) => {
            eprintln!("SKIPPING native_call benchmarks: {reason}");
            return;
        }
    };
    let clazz = java.class("rjbench.BenchLib").expect("BenchLib must resolve");

    let mut group = c.benchmark_group("native_call");
    group.bench_function("add_static", |b| {
        b.iter(|| {
            let sum: i32 = black_box(clazz.call_static("add", (1_i32, 2_i32)).expect("add"));
            black_box(sum)
        })
    });
    group.bench_function("nop_void", |b| {
        b.iter(|| {
            let _: () = clazz.call_static("nop", ()).expect("nop");
        })
    });
    group.finish();
}

fn bench_userdata_get(c: &mut Criterion) {
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING userdata_get benchmarks: {reason}");
        return;
    }
    let java = match bench_jvm() {
        Ok(j) => j.clone(),
        Err(reason) => {
            eprintln!("SKIPPING userdata_get benchmarks: {reason}");
            return;
        }
    };
    let clazz = java.class("com.example.Counter").expect("Counter must resolve");
    let counter: JObject = clazz.call_static("create", ()).expect("create shell");

    let mut group = c.benchmark_group("userdata_get");
    // A bound-object method call: attach + call machinery + the userdata
    // identity lookup (find_class System + static call + IsSameObject check)
    // + the Mutex increment.
    group.bench_function("increment_call", |b| {
        b.iter(|| {
            let v: i64 = black_box(counter.call("increment", (1_i64,)).expect("increment"));
            black_box(v)
        })
    });
    // The pure identity lookup: identityHashCode + registry + collision
    // check + downcast, no native dispatch around it.
    group.bench_function("get", |b| {
        b.iter(|| {
            let state = with_env(|env| rjava::userdata::get::<Counter>(env, &counter)).expect("get");
            black_box(state)
        })
    });
    group.finish();
}

fn bench_array_string(c: &mut Criterion) {
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING array_string benchmarks: {reason}");
        return;
    }
    let java = match bench_jvm() {
        Ok(j) => j.clone(),
        Err(reason) => {
            eprintln!("SKIPPING array_string benchmarks: {reason}");
            return;
        }
    };
    let clazz = java.class("rjbench.BenchLib").expect("BenchLib must resolve");
    let strings: Vec<String> = (0..64).map(|i| format!("string-{i}")).collect();

    let mut group = c.benchmark_group("array_string");
    // Vec<String> -> String[] (caller-side ToJava on the argument).
    group.bench_function("to_java_64", |b| {
        let input = strings.clone();
        b.iter(|| {
            let n: i64 = black_box(clazz.call_static("totalLen", (input.clone(),)).expect("totalLen"));
            black_box(n)
        })
    });
    // String[] -> Vec<String> (caller-side FromJava on the return value).
    group.bench_function("from_java_64", |b| {
        b.iter(|| {
            let out: Vec<String> =
                black_box(clazz.call_static("strings", (64_i32,)).expect("strings"));
            black_box(out.len())
        })
    });
    group.finish();
}

fn bench_array_i32(c: &mut Criterion) {
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING array_i32 benchmarks: {reason}");
        return;
    }
    let java = match bench_jvm() {
        Ok(j) => j.clone(),
        Err(reason) => {
            eprintln!("SKIPPING array_i32 benchmarks: {reason}");
            return;
        }
    };
    let clazz = java.class("rjbench.BenchLib").expect("BenchLib must resolve");
    let ints: Vec<i32> = (0..1024).collect();

    let mut group = c.benchmark_group("array_i32");
    // Vec<i32> -> int[] (caller-side ToJava on the argument).
    group.bench_function("to_java_1024", |b| {
        let input = ints.clone();
        b.iter(|| {
            let n: i64 = black_box(clazz.call_static("sum", (input.clone(),)).expect("sum"));
            black_box(n)
        })
    });
    // int[] -> Vec<i32> (caller-side FromJava on the return value).
    group.bench_function("from_java_1024", |b| {
        b.iter(|| {
            let out: Vec<i32> = black_box(clazz.call_static("range", (1024_i32,)).expect("range"));
            black_box(out.len())
        })
    });
    group.finish();
}

fn bench_string(c: &mut Criterion) {
    if let Err(reason) = register_natives() {
        eprintln!("SKIPPING string benchmarks: {reason}");
        return;
    }
    let mut group = c.benchmark_group("string");
    group.bench_function("to_java", |b| {
        b.iter(|| {
            let n = black_box(
                with_env(|env| Ok("hello, java".to_java(env)?.len())).expect("to_java"),
            );
            black_box(n)
        })
    });
    group.bench_function("from_java", |b| {
        b.iter(|| {
            let n = black_box(with_env(|env| {
                let s: rjava::jni::objects::JString = env.new_string("hello, java")?;
                let out = String::from_java(env, JValueOwned::Object(s.into()))?;
                Ok(out.len())
            })
            .expect("from_java"));
            black_box(n)
        })
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_native_call,
    bench_userdata_get,
    bench_array_string,
    bench_array_i32,
    bench_string
);
criterion_main!(benches);
