# rjava-helper

Internal helpers for rjava — currently the safe wrapper around the unsafe JNI
registration calls; future internal utilities live here too.

> **Status:** implementation detail of `rjava` — users never call this crate
> directly; `rjava::JClass::register_natives` calls it internally. **License:**
> MIT OR Apache-2.0. **Repository:** https://github.com/hongweifei/rjava

## Why a separate crate

`rjava` is `#![forbid(unsafe_code)]`, but the two JNI calls behind
native-method registration are `unsafe` in jni-rs:

* constructing a `jni::NativeMethod` via `NativeMethod::from_raw_parts`, and
* calling `jni::Env::register_native_methods`.

Both `unsafe` blocks live **here** — attributed to this crate, with `SAFETY`
comments — so `rjava`'s own source and the `native!` / `native_inst!` macro
expansions (from the `rjava-macros` crate) stay 100% `unsafe`-free.

## register_natives

`register_natives(env, class, methods)` registers a batch of native methods on
`class`. `methods` carries `(name, signature, function-pointer)` triples; each
`fn_ptr` must be the address of a trampoline whose C signature matches the JNI
signature — guaranteed by construction, since the `native!` / `native_inst!`
macros generate (explicit form) or pick (type-derived form) the trampoline for
the descriptor they produce. Errors (e.g. `NoSuchMethodError` on a signature
mismatch) are surfaced as `jni::errors::Error`s by rjava's exception
machinery.

## License

MIT OR Apache-2.0.

[rjava]: https://docs.rs/rjava/latest/rjava/
