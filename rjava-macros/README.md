# rjava-macros

Proc macros for [rjava]'s native-method feature: registering Rust functions as
Java `native` methods via JNI `RegisterNatives`.

> **Status:** implementation detail of `rjava` — users never depend on this
> crate directly; use `rjava::native` / `rjava::native_inst` via the `rjava`
> crate. **License:** MIT OR Apache-2.0. **Repository:**
> https://github.com/hongweifei/rjava

## The macros

The two macros are re-exported from the `rjava` crate root:

* [`native!`](https://docs.rs/rjava/latest/rjava/macro.native.html) — register
  a Rust function as a Java **static** `native` method; the `JClass` receiver
  is dropped.
* [`native_inst!`](https://docs.rs/rjava/latest/rjava/macro.native_inst.html) —
  register a Rust function as a Java **instance** `native` method; the `this`
  object is prepended to the argument tuple.

Each macro accepts **two forms**:

* **Type-derived** (primary): `native!("name", f)` — no signature string; the
  JNI descriptor is derived at runtime from the Rust types of `f`. Closures
  and fn items both work; a capturing closure must be `Send + Sync + 'static`.
* **Explicit-signature** (escape hatch): `native!("name", "(II)I", f)` — the
  JNI descriptor as a plain quoted string, the same form JNI itself uses. The
  signature is parsed at compile time by a small hand-written parser (not
  `syn`), and each invocation generates a unique `extern "system"` trampoline
  whose C parameter and return types are the exact `jni::sys` types.

## Safety

The expansion contains **no `unsafe`** — so a user crate that only uses these
macros can keep `#![forbid(unsafe_code)]`. The one unsafe pair in the feature
— constructing a `jni::NativeMethod` via `NativeMethod::from_raw_parts` and
calling `Env::register_native_methods` — lives in the `rjava-helper` crate's
`register_natives` helper, with `SAFETY` comments.

## Dependency note

All paths in the expansion reference `rjava` as `::rjava::…`, so the consuming
crate's dependency must be named `rjava`.

## License

MIT OR Apache-2.0.

[rjava]: https://docs.rs/rjava/latest/rjava/
