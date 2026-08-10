//! Proc macros for [rjava]: `native!` / `native_inst!` for registering Rust
//! functions as Java `native` methods via JNI `RegisterNatives`,
//! [`async_native!`] for registering Rust **async** functions whose Java
//! return type is a `java.util.concurrent.CompletableFuture`, and [`bind!`]
//! for declaring compile-time-typed bindings of Java classes.
//!
//! This crate is an implementation detail of `rjava` — you normally never
//! depend on it directly. The macros are re-exported from the `rjava` crate
//! root as `rjava::native`, `rjava::native_inst`, `rjava::async_native` and
//! `rjava::bind`.
//!
//! # What the native macros do
//!
//! Each macro accepts **two forms**:
//!
//! 1. **Type-derived** (primary): `native!("add", f)` — no signature string.
//!    The JNI descriptor is derived at runtime from the Rust types of `f`
//!    via `rjava`'s `ToJava`/`FromJava` machinery; the C ABI is fixed at
//!    compile time by shared generic trampolines in `rjava` itself (one
//!    instantiation per `(A, R)` signature). Closures **and** fn items work;
//!    a capturing closure must be `Send + Sync + 'static`:
//!
//!    ```ignore
//!    native!("add", |env, (a, b): (i32, i32)| Ok(a + b))  // int add(int, int)
//!    native!("shout", shout)                              // String shout(String)
//!    native_inst!("times", times)                         // int times(int)
//!    ```
//!
//!    Two *different* registrations with the same Rust signature share one
//!    trampoline, so the second would silently replace the first's callable;
//!    `JClass::register_natives` detects this and rejects it with a clear
//!    error — use the explicit-signature form to disambiguate.
//!
//! 2. **Explicit-signature** (escape hatch): `native!("add", "(II)I", f)` —
//!    the JNI signature as a **plain quoted string**, the same form JNI
//!    itself uses. Each invocation generates a *unique* trampoline, so any
//!    number of registrations may share a signature:
//!
//!    ```ignore
//!    native!("add", "(II)I", add)                                // int add(int, int)
//!    native_inst!("times", "(I)I", times)                       // int times(int)
//!    native!("shout", "(Ljava/lang/String;)Ljava/lang/String;", shout)
//!    native!("avg", "([D)D", avg)                               // double avg(double[])
//!    native!("range", "(I)[I", range)                           // int[] range(int)
//!    native!("fail", "(Ljava/lang/String;)V", fail)             // void fail(String)
//!    ```
//!
//! The signature string of the explicit form is parsed **at compile time** by
//! this crate with a small hand-written parser (not `syn`): `(<params>)<ret>`
//! where each parameter and the return type is one of `Z B C S I J F D`
//! (boolean, byte, char, short, int, long, float, double), `L<name>;` (a
//! reference type — the name may contain `/` or `.`; dots are converted to
//! slashes in the generated descriptor), or `[<type>]` (an array of a
//! primitive or of a class). `V` is only valid as the return type. Up to the
//! JVM's method-descriptor limit: 255 parameter units for static methods,
//! 254 for instance methods (`this` takes one of the 255), where `long` and
//! `double` count as two units each (JVMS §4.3.3); real-world methods rarely
//! exceed ~20 parameters. Anything the grammar cannot parse is a `syn::Error`
//! with a
//! helpful message that includes the offending signature and a valid example.
//!
//! The macro then emits an `extern "system"` trampoline whose C parameter and
//! return types are the exact `jni::sys` types for the parsed signature, and
//! a [`rjava::NativeMethod`](https://docs.rs/rjava/latest/rjava/struct.NativeMethod.html) that points at it. At call time the trampoline
//! converts the raw JNI arguments into [`rjava::JavaArg`](https://docs.rs/rjava/latest/rjava/enum.JavaArg.html)s and hands them to
//! [`rjava::native::dispatch`](https://docs.rs/rjava/latest/rjava/native/fn.dispatch.html), which runs the user's Rust function. Panics
//! are caught and converted into Java `RuntimeException`s; `Err(…)` results
//! are thrown as Java exceptions too, so a panic or error never crosses the
//! FFI boundary. The type-derived form's expansion is a single call to
//! `rjava::native::make_native_*`, which does the same dispatch at call time
//! through the shared trampoline.
//!
//! # What `async_native!` does
//!
//! [`async_native!`] is the async twin of the type-derived `native!` form:
//! `async_native!("name", f)` registers a Rust **async** function (fn item
//! **or** closure returning a `Future`) as a Java static `native` method
//! whose Java return type is `java.util.concurrent.CompletableFuture` — the
//! Rust async work runs on a detached std thread and completes the future
//! later, so Java code can `await`/chain it. The async function receives a
//! [`rjava::Java`](https://docs.rs/rjava/latest/rjava/struct.Java.html) facade **by value** (not an `Env`: `Env` is not `Send` and
//! cannot survive an `await`) plus the argument tuple, and returns
//! `JavaResult<R>`. **v1 has only the type-derived form** — the
//! explicit-signature form is not available for async natives (two
//! registrations with the same Rust argument-tuple type share one trampoline
//! and are rejected at `register_natives` time; there is no escape hatch
//! yet). See the `rjava::future` module docs for the full semantics.
//!
//! ```ignore
//! async fn compute(java: Java, (a, b): (i32, i32)) -> JavaResult<i32> { Ok(a + b) }
//! // Java: `public static native CompletableFuture<Integer> compute(int a, int b);`
//! clazz.register_natives(&[async_native!("compute", compute)?])?;
//! ```
//!
//! # Safety
//!
//! The **expansion contains no `unsafe`** — the trampoline body (explicit
//! form) is entirely safe code, and the type-derived expansion is one plain
//! function call, so a user crate that only *uses* these macros can keep
//! `#![forbid(unsafe_code)]` (the integration tests of `rjava` itself do
//! exactly that). The one remaining `unsafe` in the feature — constructing a
//! `jni::NativeMethod` from a raw function pointer
//! (`NativeMethod::from_raw_parts`) and calling
//! `Env::register_native_methods` — lives in the `rjava-helper` crate's
//! `register_natives` helper, with `SAFETY` comments. The `EnvUnowned::with_env`
//! outcome's non-`Ok` arms (its own `catch_unwind` already covers the panics
//! of `dispatch`) are handled by returning the signature's default value —
//! rare, and acceptable: the JVM simply observes a default return value.
//!
//! All paths in the expansion reference `rjava` as `::rjava::…`, so **the
//! dependency must be named `rjava`** in the consuming crate.
//!
//! # What `bind!` does
//!
//! [`bind!`] declares a compile-time-typed binding for a Java class: the
//! class name, a wrapper name, and the methods with their Rust types. The
//! JNI descriptors are computed at **macro time** (mirroring `rjava`'s
//! `ToJava`/`FromJava` machinery) and the expansion — a wrapper `struct`,
//! one typed method per declaration, and a `rjava::bind::JavaBound` impl
//! with a per-wrapper class cache — is plain safe code, so
//! `#![forbid(unsafe_code)]` user crates keep compiling. Method IDs are
//! re-derived per call (jni-rs `MethodID` carries an `Env` lifetime); the
//! class reference is cached.
//!
//! [rjava]: https://docs.rs/rjava

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use std::collections::HashSet;
use syn::parse::{Parse, ParseStream};
use syn::spanned::Spanned;
use syn::{braced, bracketed, parenthesized, parse_macro_input, Expr, Ident, LitStr, Token, Type};

/// The parsed input of `native!` / `native_inst!`: either `name_lit, f` (the
/// **type-derived** form — the JNI signature is computed from the Rust types)
/// or `name_lit, sig_lit, f` (the **explicit-signature** form).
/// A function-like proc macro receives the tokens between the invocation
/// delimiters, so there is no outer paren pair to parse.
struct Input {
    name: LitStr,
    sig: Option<LitStr>,
    f: Expr,
}

impl Parse for Input {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let name: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        // A second string literal means the explicit-signature form; anything
        // else (a closure `|…|`, a `move` keyword, a path to a fn item) is
        // the type-derived form. The explicit form has a comma between the
        // signature and `f`; the derived form goes straight to `f`.
        let sig = if input.peek(LitStr) {
            let sig: LitStr = input.parse()?;
            input.parse::<Token![,]>()?;
            Some(sig)
        } else {
            None
        };
        let f: Expr = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after `name[, sig], f`"));
        }
        Ok(Input { name, sig, f })
    }
}

/// Register a Rust function as a Java **static** `native` method.
///
/// The raw `JClass` receiver that the JVM passes to static native methods is
/// dropped; the function receives only the method's arguments.
///
/// # Syntax
///
/// Two forms; the **type-derived** form is primary:
///
/// ```ignore
/// native!("methodName", f)            // type-derived: no signature string
/// native!("methodName", "(II)I", f)   // explicit-signature form
/// ```
///
/// * `"methodName"` — the Java method name (string literal).
/// * `f` — the Rust function to call, either a **fn item** or a **closure**
///   with the signature
///   `fn(&mut jni::Env, args_tuple) -> rjava::JavaResult<R>`, where `R` is one
///   of the single-value [`rjava::ToJava`](https://docs.rs/rjava/latest/rjava/trait.ToJava.html) types. A capturing closure must
///   be `Send + Sync + 'static` (use `move` for stack captures).
///
///   In the type-derived form the JNI descriptor is derived from `f`'s Rust
///   types at runtime — no signature string. Two *different* registrations
///   with the same Rust signature share one trampoline and are rejected at
///   [`JClass::register_natives`](https://docs.rs/rjava/latest/rjava/struct.JClass.html#method.register_natives) with a clear error; the explicit
///   form (below) is the escape hatch in that situation.
///
/// * `"(II)I"` — *explicit form only*: the JNI method descriptor as a
///   **quoted string** (e.g. `"(Ljava/lang/String;)V"`, `"([D)D"`). It is
///   parsed at compile time and the trampoline's C types are derived from it;
///   malformed signatures are a compile error with a helpful message.
///
/// # Example
///
/// ```ignore
/// use rjava::prelude::*;
/// use rjava::native;
///
/// // Java: `public static native int add(int a, int b);`
/// fn add(_env: &mut jni::Env, (a, b): (i32, i32)) -> JavaResult<i32> {
///     Ok(a + b)
/// }
///
/// let clazz = java.class("com.example.NativeLib")?;
/// clazz.register_natives(&[native!("add", add)?])?;            // derived
/// clazz.register_natives(&[native!("add", "(II)I", add)?])?;   // explicit
/// ```
///
/// The macro expands to a `(|| { … })()` expression returning
/// `rjava::JavaResult<rjava::NativeMethod>`, so multiple invocations never
/// collide and the `?` operator works. The expansion is `unsafe`-free.
#[proc_macro]
pub fn native(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Input);
    match &input.sig {
        Some(_) => expand(&input, false).into(),
        None => expand_derived(&input, false).into(),
    }
}

/// Register a Rust function as a Java **instance** `native` method.
///
/// The `this` object that the JVM passes to instance native methods is
/// prepended to the argument list, so the Rust function's first tuple element
/// receives a [`rjava::JObject`](https://docs.rs/rjava/latest/rjava/struct.JObject.html) handle for the receiver:
///
/// ```ignore
/// use rjava::prelude::*;
/// use rjava::native_inst;
///
/// // Java: `public native int times(int factor);`
/// fn times(_env: &mut jni::Env, (this, factor): (JObject, i32)) -> JavaResult<i32> {
///     let base: i32 = this.get_field("base")?;
///     Ok(base * factor)
/// }
///
/// let clazz = java.class("com.example.NativeLib")?;
/// clazz.register_natives(&[native_inst!("times", times)?])?;          // derived
/// clazz.register_natives(&[native_inst!("times", "(I)I", times)?])?;  // explicit
/// ```
///
/// Like [`native`], the macro accepts both the type-derived form
/// (`native_inst!("name", f)` — fn item or closure; the JNI signature is
/// derived from the Rust types) and the explicit-signature form
/// (`native_inst!("name", "(I)I", f)`). Everything else (signature grammar,
/// `unsafe`-free expansion, error and panic handling) is identical to
/// [`native`].
#[proc_macro]
pub fn native_inst(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Input);
    match &input.sig {
        Some(_) => expand(&input, true).into(),
        None => expand_derived(&input, true).into(),
    }
}

// ---------------------------------------------------------------------------

/// The parsed input of `async_native!`: `name_lit, f` — the **type-derived**
/// form only in v1 (no signature string; the explicit-signature form is not
/// available for async natives yet).
struct AsyncInput {
    name: LitStr,
    f: Expr,
}

impl Parse for AsyncInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let name: LitStr = input.parse()?;
        input.parse::<Token![,]>()?;
        let f: Expr = input.parse()?;
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after `name, f`"));
        }
        Ok(AsyncInput { name, f })
    }
}

/// Register a Rust **async** function as a Java **static** `native` method
/// whose Java return type is `java.util.concurrent.CompletableFuture` — the
/// async twin of the type-derived [`native!`] form.
///
/// Java code calls the method and gets a `CompletableFuture` it can chain
/// and `await`; the Rust async work runs on a detached std thread and
/// completes the future (or completes it exceptionally) when it finishes.
/// See the [`rjava::future`](https://docs.rs/rjava/latest/rjava/future/index.html)
/// module docs for the full semantics.
///
/// # Syntax
///
/// ```ignore
/// async_native!("methodName", f)
/// ```
///
/// * `"methodName"` — the Java method name (string literal).
/// * `f` — the Rust async function to call, either a **fn item** or a
///   **closure returning a `Future`**:
///
///   ```ignore
///   async fn compute(java: Java, (a, b): (i32, i32)) -> JavaResult<i32> { Ok(a + b) }
///   // fn item form:
///   async_native!("compute", compute)?,
///   // closure form:
///   async_native!("compute", |java: Java, (a, b): (i32, i32)| async move { Ok(a + b) })?,
///   ```
///
///   The function receives a [`rjava::Java`](https://docs.rs/rjava/latest/rjava/struct.Java.html)
///   facade **by value** (clonable, `Send + Sync` — use its methods to call
///   back into Java; do **not** take an `Env`, which is not `Send` and
///   cannot survive an `await`) plus the argument tuple, and returns
///   `rjava::JavaResult<R>`. A capturing closure must be
///   `Send + Sync + 'static` (use `move` for stack captures).
///
///   The JNI parameter descriptor is derived from the Rust argument-tuple
///   type at runtime (no signature string); the Java return descriptor is
///   always exactly `Ljava/util/concurrent/CompletableFuture;`, so the Java
///   declaration must be `public static native CompletableFuture<R> name(...)`.
///   Two *different* registrations with the **same Rust argument-tuple
///   type** share one trampoline and are rejected at
///   [`JClass::register_natives`](https://docs.rs/rjava/latest/rjava/struct.JClass.html#method.register_natives)
///   with a clear error. **The explicit-signature escape hatch is not
///   available for `async_native!` in this version** — register only one
///   async method per Rust argument-tuple type.
///
/// # Example
///
/// ```ignore
/// use rjava::prelude::*;
/// use rjava::async_native;
///
/// // Java: `public static native CompletableFuture<Integer> compute(int a, int b);`
/// async fn compute(java: Java, (a, b): (i32, i32)) -> JavaResult<i32> {
///     let _ = java; // call back into Java through `java` before any await
///     Ok(a + b)
/// }
///
/// let clazz = java.class("com.example.NativeLib")?;
/// clazz.register_natives(&[async_native!("compute", compute)?])?;
/// // Java (and Rust) callers now get a CompletableFuture<Integer> that the
/// // Rust async work completes asynchronously:
/// let cf: JObject = clazz.call_static("compute", (1_i32, 2_i32))?;
/// let sum: i32 = rjava::future::java_future::<i32>(java, cf).await?; // == 3
/// ```
///
/// The macro expands to a `(|| { … })()` expression returning
/// `rjava::JavaResult<rjava::NativeMethod>`, so multiple invocations never
/// collide and the `?` operator works. The expansion is `unsafe`-free.
#[proc_macro]
pub fn async_native(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as AsyncInput);
    expand_async_derived(&input).into()
}

// ---------------------------------------------------------------------------
// bind! — compile-time-typed class bindings
// ---------------------------------------------------------------------------

/// The parsed input of `bind!` (and `interface!`): `"class.Name" => Wrapper { items }`.
///
/// The class name is a string literal (dotted or slash form); the wrapper
/// name is the Rust item the macro generates; the items are `fn` method
/// declarations (optionally `static fn` for Java static methods) and `field`
/// declarations (optionally `static field` for Java static fields). The same
/// parser serves both macros — `interface!` rejects the forms it does not
/// support (fields, statics) with targeted messages.
struct BindInput {
    class: LitStr,
    name: Ident,
    methods: Vec<BindMethod>,
    fields: Vec<BindField>,
}

/// One method declaration inside a [`BindInput`]:
/// `[static] fn name(params) -> ret [java_name = "JavaName"];`.
///
/// `ret` is `None` when the return type is omitted — the `interface!` void
/// sugar (`fn ping();`); `bind!` rejects the omission with a clear message
/// (its grammar requires an explicit `-> Ret`).
struct BindMethod {
    is_static: bool,
    name: Ident,
    params: Vec<(Ident, Type)>,
    ret: Option<Type>,
    /// The optional `[java_name = "…"]` alias: the Java-side name when it
    /// differs from the Rust `name` (e.g. `to_string` → `toString`).
    java_name: Option<LitStr>,
}

/// One field declaration inside a [`BindInput`]: `[static] field name: Type;`.
struct BindField {
    is_static: bool,
    name: Ident,
    ty: Type,
}

impl Parse for BindInput {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let class: LitStr = input.parse()?;
        input.parse::<Token![=>]>()?;
        let name: Ident = input.parse()?;
        let body;
        braced!(body in input);
        let mut methods = Vec::new();
        let mut fields = Vec::new();
        while !body.is_empty() {
            let is_static = body.peek(Token![static]);
            if is_static {
                body.parse::<Token![static]>()?;
            }
            if !body.peek(Token![fn]) {
                // `field name: Type;` — `fn` is a keyword so the peek is
                // exact; anything else here is `field` or a typo.
                let kw: Ident = body.parse()?;
                if kw != "field" {
                    return Err(body.error(
                        "expected `fn`, `static fn`, `field` or `static field`",
                    ));
                }
                let name: Ident = body.parse()?;
                body.parse::<Token![:]>()?;
                let ty: Type = body.parse()?;
                body.parse::<Token![;]>()?;
                fields.push(BindField { is_static, name, ty });
                continue;
            }
            body.parse::<Token![fn]>()?;
            let name: Ident = body.parse()?;
            let params_content;
            parenthesized!(params_content in body);
            let mut params = Vec::new();
            while !params_content.is_empty() {
                let pname: Ident = params_content.parse()?;
                params_content.parse::<Token![:]>()?;
                let pty: Type = params_content.parse()?;
                params.push((pname, pty));
                if params_content.is_empty() {
                    break;
                }
                params_content.parse::<Token![,]>()?;
            }
            // The return type may be omitted (`fn ping();` — the interface!
            // void sugar); `bind!` rejects the omission in its expansion.
            let ret = if body.peek(Token![->]) {
                body.parse::<Token![->]>()?;
                Some(body.parse()?)
            } else {
                None
            };
            // Optional `[java_name = "JavaName"]` alias between the return
            // type and the `;`.
            let mut java_name = None;
            if body.peek(syn::token::Bracket) {
                let attr;
                bracketed!(attr in body);
                let key: Ident = attr.parse()?;
                if key != "java_name" {
                    return Err(attr.error(format!(
                        "expected `java_name = \"…\"`, found `{key}`"
                    )));
                }
                attr.parse::<Token![=]>()?;
                java_name = Some(attr.parse::<LitStr>()?);
                if !attr.is_empty() {
                    return Err(attr.error(
                        "unexpected tokens after `java_name = \"…\"`",
                    ));
                }
            }
            body.parse::<Token![;]>()?;
            methods.push(BindMethod {
                is_static,
                name,
                params,
                ret,
                java_name,
            });
        }
        if !input.is_empty() {
            return Err(input.error("unexpected tokens after the declaration"));
        }
        Ok(BindInput {
            class,
            name,
            methods,
            fields,
        })
    }
}

/// The macro-time mapping of a declared Rust type to its JNI descriptor
/// fragment — mirroring `rjava`'s `ToJava`/`FromJava` machinery (see the
/// `convert` module in the `rjava` crate) so the generated signature literal
/// needs no runtime derivation.
enum Mapped {
    /// A primitive letter (`Z B C S I J F D`, and `V` for the void return).
    Prim(char),
    /// `String` / `&str` → `Ljava/lang/String;`.
    Str,
    /// `JObject` → `Ljava/lang/Object;` (deliberately generic — the exact
    /// signature is resolved at runtime by the reflection fallback when the
    /// real type differs).
    Object,
    /// `Self` — the bound class; valid as a return type only.
    SelfRef,
    /// `Option<T>` → the descriptor of `T` (null-tolerant).
    OptionOf(Box<Mapped>),
    /// `Vec<T>` → `[` + the descriptor of `T`.
    VecOf(Box<Mapped>),
}

const SUPPORTED_TYPES: &str = "supported types: () (return only), bool, i8, i16, i32, i64, f32, \
     f64, char, u8 (Java byte), String, &str (parameters only), JObject, Option<T>, Vec<T>, and \
     Self (return only)";

/// Map a declared Rust type to its [`Mapped`] JNI fragment.
fn map_type(ty: &Type) -> syn::Result<Mapped> {
    match ty {
        Type::Path(path) if path.qself.is_none() => {
            let Some(seg) = path.path.segments.last() else {
                return Err(syn::Error::new(ty.span(), format!("unsupported type in bind!; {SUPPORTED_TYPES}")));
            };
            let name = seg.ident.to_string();
            Ok(match name.as_str() {
                "Self" => Mapped::SelfRef,
                "i8" | "u8" => Mapped::Prim('B'),
                "i16" => Mapped::Prim('S'),
                "i32" => Mapped::Prim('I'),
                "i64" => Mapped::Prim('J'),
                "f32" => Mapped::Prim('F'),
                "f64" => Mapped::Prim('D'),
                "bool" => Mapped::Prim('Z'),
                "char" => Mapped::Prim('C'),
                "String" => Mapped::Str,
                "JObject" => Mapped::Object,
                "Option" => Mapped::OptionOf(Box::new(map_type(single_generic(seg, ty, "Option")?)?)),
                "Vec" => Mapped::VecOf(Box::new(map_type(single_generic(seg, ty, "Vec")?)?)),
                _ => {
                    return Err(syn::Error::new(
                        ty.span(),
                        format!("unsupported type `{name}` in bind!; {SUPPORTED_TYPES}"),
                    ))
                }
            })
        }
        // `&str` — the one supported reference type (a `String` argument).
        Type::Reference(r) if r.mutability.is_none() => {
            if let Type::Path(p) = r.elem.as_ref()
                && p.qself.is_none()
                && p.path.segments.len() == 1
                && p.path.segments[0].ident == "str"
            {
                Ok(Mapped::Str)
            } else {
                Err(syn::Error::new(ty.span(), format!("unsupported reference type in bind!; {SUPPORTED_TYPES}")))
            }
        }
        // `()` — the void return type.
        Type::Tuple(t) if t.elems.is_empty() => Ok(Mapped::Prim('V')),
        _ => Err(syn::Error::new(ty.span(), format!("unsupported type in bind!; {SUPPORTED_TYPES}"))),
    }
}

/// The single type argument of `Option<T>` / `Vec<T>`.
fn single_generic<'a>(seg: &'a syn::PathSegment, ty: &Type, name: &str) -> syn::Result<&'a Type> {
    match &seg.arguments {
        syn::PathArguments::AngleBracketed(args) if args.args.len() == 1 => {
            match args.args.first().expect("length checked above") {
                syn::GenericArgument::Type(t) => Ok(t),
                other => Err(syn::Error::new(
                    other.span(),
                    format!("`{name}<T>` requires a type argument"),
                )),
            }
        }
        _ => Err(syn::Error::new(
            ty.span(),
            format!("`{name}<T>` requires exactly one type argument"),
        )),
    }
}

/// Does this mapping contain a `Self` anywhere? (`Option<Self>` / `Vec<Self>`
/// are rejected — `Self` is only valid as a top-level return type.)
fn contains_self(m: &Mapped) -> bool {
    match m {
        Mapped::SelfRef => true,
        Mapped::OptionOf(inner) | Mapped::VecOf(inner) => contains_self(inner),
        _ => false,
    }
}

/// The JNI descriptor fragment of a mapping (parameter or return position).
fn frag(m: &Mapped, slash_class: &str) -> String {
    match m {
        Mapped::Prim(c) => c.to_string(),
        Mapped::Str => "Ljava/lang/String;".to_string(),
        Mapped::Object => "Ljava/lang/Object;".to_string(),
        Mapped::SelfRef => format!("L{slash_class};"),
        Mapped::OptionOf(inner) => frag(inner, slash_class),
        Mapped::VecOf(inner) => format!("[{}", frag(inner, slash_class)),
    }
}

/// Validate a parameter-position mapping.
fn validate_param(m: &Mapped, ty: &Type) -> syn::Result<()> {
    match m {
        Mapped::SelfRef => Err(syn::Error::new(
            ty.span(),
            "`Self` is only valid as a return type in bind!",
        )),
        Mapped::Prim('V') => Err(syn::Error::new(
            ty.span(),
            "`()` (void) is only valid as a return type in bind!",
        )),
        _ => Ok(()),
    }
}

/// Validate a return-position mapping.
fn validate_return(m: &Mapped, ty: &Type) -> syn::Result<()> {
    // `&str` has no `FromJava` in rjava — only `String` can be a return type.
    if let Type::Reference(_) = ty {
        return Err(syn::Error::new(
            ty.span(),
            "`&str` cannot be a return type in bind! — use `String`",
        ));
    }
    if let Mapped::OptionOf(inner) | Mapped::VecOf(inner) = m
        && contains_self(inner)
    {
        return Err(syn::Error::new(
            ty.span(),
            "`Option<Self>` / `Vec<Self>` are not supported in bind! — use `Option<JObject>` / `JObject`",
        ));
    }
    Ok(())
}

/// Validate a field-type mapping: no `Self`, no `()` and no `&str` (a field
/// value must be owned and concrete — `call::get_field` derives the JNI
/// signature from `FromJava::java_return_type`).
fn validate_field(m: &Mapped, ty: &Type) -> syn::Result<()> {
    match m {
        Mapped::SelfRef => Err(syn::Error::new(
            ty.span(),
            "`Self` is not supported as a field type in bind! — use a concrete type",
        )),
        Mapped::Prim('V') => Err(syn::Error::new(
            ty.span(),
            "`()` (void) is not a valid field type in bind!",
        )),
        _ => {
            if let Type::Reference(_) = ty {
                return Err(syn::Error::new(
                    ty.span(),
                    "`&str` cannot be a field type in bind! — use `String`",
                ));
            }
            if let Mapped::OptionOf(inner) | Mapped::VecOf(inner) = m
                && contains_self(inner)
            {
                return Err(syn::Error::new(
                    ty.span(),
                    "`Option<Self>` / `Vec<Self>` are not supported as field types in bind!",
                ));
            }
            Ok(())
        }
    }
}

/// CamelCase a field name with the crate's simple word-boundary rule — the
/// exact rule the `bean` module uses for its accessor names (`user_id` →
/// `UserId`, `id` → `Id`, no acronym special-casing). Used for the bool
/// field getter's `get<Name>` / `is<Name>` accessor fallback.
fn camel_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut word_start = true;
    for ch in name.chars() {
        if ch == '_' {
            word_start = true;
        } else if word_start {
            out.extend(ch.to_uppercase());
            word_start = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// The full expansion of a `bind!` invocation: the wrapper struct, one typed
/// method per declared method, and the [`rjava::bind::JavaBound`] impl with
/// the per-wrapper class cache. Every path references `::rjava::…`, so the
/// expansion is `unsafe`-free and the consuming crate must name the
/// dependency `rjava`.
fn expand_bind(input: &BindInput) -> syn::Result<TokenStream2> {
    let class_dotted = input.class.value();
    if class_dotted.is_empty() {
        return Err(syn::Error::new(
            input.class.span(),
            "the class name must not be empty",
        ));
    }
    let slash = if class_dotted.contains('/') {
        class_dotted.clone()
    } else {
        class_dotted.replace('.', "/")
    };
    let slash_lit = LitStr::new(&slash, input.class.span());
    let class_doc = format!(
        "A compile-time-typed binding for the Java class `{class_dotted}` (`{slash}`)."
    );
    let wrapper = &input.name;

    let mut method_items = Vec::new();
    let mut seen = HashSet::new();
    let mut seen_java = HashSet::new();
    for m in &input.methods {
        let name = &m.name;
        let mname = name.to_string();
        if !seen.insert(mname.clone()) {
            return Err(syn::Error::new(
                m.name.span(),
                format!(
                    "duplicate method `{mname}` in bind! — Rust does not support overloading, so \
                     a Java overload can be bound only once; keep the other overloads on the \
                     dynamic `call` / `call_static` path"
                ),
            ));
        }
        // The Java-side name: the `[java_name = "…"]` alias when given, the
        // Rust ident verbatim otherwise.
        let java_name = m
            .java_name
            .as_ref()
            .map(|l| l.value())
            .unwrap_or_else(|| mname.clone());
        if java_name.is_empty() {
            return Err(syn::Error::new(
                m.name.span(),
                "`java_name = \"…\"` must not be empty",
            ));
        }
        if !seen_java.insert(java_name.clone()) {
            return Err(syn::Error::new(
                m.java_name.as_ref().map_or(m.name.span(), |l| l.span()),
                format!(
                    "duplicate Java method name `{java_name}` in bind! after \
                     `[java_name = \"…\"]` — every bound method must target a distinct Java method"
                ),
            ));
        }

        let mut params_frags = Vec::new();
        let mut param_decls = Vec::new();
        let mut arg_tys = Vec::new();
        let mut arg_names = Vec::new();
        for (pname, pty) in &m.params {
            let mapped = map_type(pty)?;
            validate_param(&mapped, pty)?;
            params_frags.push(frag(&mapped, &slash));
            param_decls.push(quote! { #pname: #pty });
            arg_tys.push(pty);
            arg_names.push(pname);
        }
        let ret = m.ret.as_ref().ok_or_else(|| {
            syn::Error::new(
                m.name.span(),
                "bind! requires an explicit return type — write `fn ping() -> ();` for a \
                 void method (only interface! allows the `fn ping();` shorthand)",
            )
        })?;
        let ret_mapped = map_type(ret)?;
        validate_return(&ret_mapped, ret)?;
        let ret_frag = frag(&ret_mapped, &slash);
        let ret_ty = ret;
        let sig_str = format!("({}){}", params_frags.join(""), ret_frag);
        let sig_lit = LitStr::new(&sig_str, m.name.span());
        let name_lit = LitStr::new(&java_name, m.java_name.as_ref().map_or(m.name.span(), |l| l.span()));

        let tuple_ty: TokenStream2 = if arg_tys.is_empty() {
            quote! { () }
        } else {
            quote! { ( #(#arg_tys),* , ) }
        };
        let tuple_val: TokenStream2 = if arg_names.is_empty() {
            quote! { () }
        } else {
            quote! { ( #(#arg_names),* , ) }
        };

        let doc = if java_name == mname {
            format!(
                "Calls the Java {} method `{mname}` with JNI signature `{sig_str}`.",
                if m.is_static { "static" } else { "instance" }
            )
        } else {
            format!(
                "Calls the Java {} method `{java_name}` (bound as `{mname}` via \
                 `[java_name = \"{java_name}\"]`) with JNI signature `{sig_str}`.",
                if m.is_static { "static" } else { "instance" }
            )
        };
        let jni_sig = quote! { ::rjava::jni::jni_sig!(jni = ::rjava::jni, sig = #sig_lit) };

        let call = if matches!(ret_mapped, Mapped::SelfRef) {
            if m.is_static {
                quote! { ::rjava::bind::static_self::<Self, #tuple_ty> }
            } else {
                quote! { ::rjava::bind::instance_self::<Self, #tuple_ty> }
            }
        } else if m.is_static {
            quote! { ::rjava::bind::static_method::<Self, #ret_ty, #tuple_ty> }
        } else {
            quote! { ::rjava::bind::instance_method::<Self, #ret_ty, #tuple_ty> }
        };

        let item = if m.is_static {
            // Static methods receive the `Java` facade explicitly (there is
            // no receiver object).
            quote! {
                #[doc = #doc]
                pub fn #name(java: &::rjava::Java, #(#param_decls),*) -> ::rjava::JavaResult<#ret_ty> {
                    #call(java, #name_lit, #jni_sig, #tuple_val)
                }
            }
        } else {
            quote! {
                #[doc = #doc]
                pub fn #name(&self, #(#param_decls),*) -> ::rjava::JavaResult<#ret_ty> {
                    #call(&self.java, &self.obj, #name_lit, #jni_sig, #tuple_val)
                }
            }
        };
        method_items.push(item);
    }

    // Fields: one typed getter + setter pair per declaration. The accessor
    // names mirror the bean module's `get<Name>`/`set<Name>` convention
    // (`field label: String;` → `get_label` / `set_label`); the *Java* field
    // name is the declared ident verbatim (like methods). A `bool` field's
    // getter falls back to the bean-style accessor methods `get<Name>` then
    // `is<Name>` when the field itself does not exist — mirroring the bean
    // read path. Static fields generate `get_static_<name>` / `set_static_<name>`
    // taking `java: &Java`.
    for f in &input.fields {
        let fname = f.name.to_string();
        let fty = &f.ty;
        let mapped = map_type(fty)?;
        validate_field(&mapped, fty)?;
        let field_lit = LitStr::new(&fname, f.name.span());
        let accessor_base = fname.to_lowercase();
        let getter = format_ident!("get_{accessor_base}");
        let setter = format_ident!("set_{accessor_base}");
        let jni_frag = frag(&mapped, &slash);
        let doc_kind = if f.is_static { "static" } else { "instance" };
        let get_doc = format!(
            "Reads the Java {doc_kind} field `{fname}` (JNI type `{jni_frag}`)."
        );
        let set_doc = format!(
            "Writes the Java {doc_kind} field `{fname}` (JNI type `{jni_frag}`)."
        );
        if f.is_static {
            let getter = format_ident!("get_static_{accessor_base}");
            let setter = format_ident!("set_static_{accessor_base}");
            method_items.push(quote! {
                #[doc = #get_doc]
                pub fn #getter(java: &::rjava::Java) -> ::rjava::JavaResult<#fty> {
                    ::rjava::bind::static_field_get::<Self, #fty>(java, #field_lit)
                }
                #[doc = #set_doc]
                pub fn #setter(java: &::rjava::Java, v: #fty) -> ::rjava::JavaResult<()> {
                    ::rjava::bind::static_field_set::<Self, #fty>(java, #field_lit, v)
                }
            });
        } else if matches!(mapped, Mapped::Prim('Z')) {
            // A bool field: the raw field first, then the bean-style
            // accessor methods (see the bind module docs).
            let bool_get_doc = format!(
                "Reads the Java instance field `{fname}` — or, when the field does not exist, \
                 the bean-style accessor methods `get{camel}` / `is{camel}` (mirroring the bean \
                 read path).",
                camel = crate::camel_case(&fname)
            );
            method_items.push(quote! {
                #[doc = #bool_get_doc]
                pub fn #getter(&self) -> ::rjava::JavaResult<bool> {
                    ::rjava::bind::instance_bool_field_get::<Self>(&self.java, &self.obj, #field_lit)
                }
                #[doc = #set_doc]
                pub fn #setter(&self, v: bool) -> ::rjava::JavaResult<()> {
                    ::rjava::bind::instance_field_set::<Self, bool>(&self.java, &self.obj, #field_lit, v)
                }
            });
        } else {
            method_items.push(quote! {
                #[doc = #get_doc]
                pub fn #getter(&self) -> ::rjava::JavaResult<#fty> {
                    ::rjava::bind::instance_field_get::<Self, #fty>(&self.java, &self.obj, #field_lit)
                }
                #[doc = #set_doc]
                pub fn #setter(&self, v: #fty) -> ::rjava::JavaResult<()> {
                    ::rjava::bind::instance_field_set::<Self, #fty>(&self.java, &self.obj, #field_lit, v)
                }
            });
        }
    }

    Ok(quote! {
        #[allow(non_camel_case_types)]
        #[doc = #class_doc]
        pub struct #wrapper {
            java: ::rjava::Java,
            obj: ::rjava::JObject,
        }

        #[allow(non_snake_case)]
        impl #wrapper {
            #(#method_items)*
        }

        impl ::rjava::bind::JavaBound for #wrapper {
            fn class_name() -> &'static str {
                #slash_lit
            }
            fn class(
                env: &mut ::rjava::jni::Env<'_>,
            ) -> ::rjava::JavaResult<
                &'static ::rjava::jni::objects::Global<::rjava::jni::objects::JClass<'static>>,
            > {
                // Per-wrapper-type cache: `OnceLock<Global>` is `Send + Sync`,
                // so the cache needs no `unsafe` (the `userdata` module's
                // `SYSTEM_CLASS` uses the same pattern). The first caller does
                // the lookup; the loser of the `get_or_init` race drops its
                // redundant reference. A wrong class name surfaces here — as
                // a clear error from the first actual call — and is *not*
                // cached, so every call retries until the name resolves.
                static CLASS: ::std::sync::OnceLock<
                    ::rjava::jni::objects::Global<::rjava::jni::objects::JClass<'static>>,
                > = ::std::sync::OnceLock::new();
                if let Some(class) = CLASS.get() {
                    return Ok(class);
                }
                let class = ::rjava::bind::find_class_global(env, Self::class_name())?;
                Ok(CLASS.get_or_init(|| class))
            }
            fn wrap(java: ::rjava::Java, obj: ::rjava::JObject) -> Self {
                #wrapper { java, obj }
            }
            fn obj(&self) -> &::rjava::JObject {
                &self.obj
            }
            fn into_obj(self) -> ::rjava::JObject {
                self.obj
            }
        }
    })
}

/// Declare a compile-time-typed binding for a Java class.
///
/// The dynamic call path (`java.new_object(...)` + `obj.call(name, args)`)
/// is string-typed and runtime-checked; a binding declares the class and its
/// methods **once**, with Rust types, and every call becomes a direct typed
/// method with typed arguments and a typed return:
///
/// ```ignore
/// rjava::bind! {
///     "java.lang.StringBuilder" => StringBuilder {
///         fn append(s: &str) -> Self;     // instance method; `-> Self` re-wraps
///         fn length() -> i32;             //   the same object (chainable)
///         fn toString() -> String;
///         static fn valueOf(b: bool) -> String;  // static method
///     }
/// }
///
/// let sb = java.new::<StringBuilder>(("Hello",))?;
/// sb.append(" world")?;
/// let len: i32 = sb.length()?;
/// ```
///
/// * `"java.lang.StringBuilder"` — the Java class name, dotted or slash form.
/// * `StringBuilder` — the name of the generated wrapper `pub struct`
///   (private fields: the [`Java`](https://docs.rs/rjava/latest/rjava/struct.Java.html)
///   facade and the [`JObject`](https://docs.rs/rjava/latest/rjava/struct.JObject.html) handle).
/// * `fn name(T1, T2) -> R;` — an instance method: generated as
///   `pub fn name(&self, t1: T1, t2: T2) -> rjava::JavaResult<R>`.
/// * `static fn name(T1) -> R;` — a Java static method: generated as
///   `pub fn name(java: &rjava::Java, t1: T1) -> rjava::JavaResult<R>`.
/// * `-> Self` — the Java method returns the bound class; the returned
///   object is re-wrapped (chainable). `Self` is not valid in parameter
///   position.
/// * `fn to_string() -> String [java_name = "toString"];` — the optional
///   Java-name alias: the Rust method keeps its name, the JNI call targets
///   the aliased Java method. Without an alias the Java name is the Rust
///   name verbatim.
/// * `field name: Type;` — a Java instance field: generated as
///   `pub fn get_name(&self) -> rjava::JavaResult<Type>` and
///   `pub fn set_name(&self, v: Type) -> rjava::JavaResult<()>` over the
///   Java field named exactly `name`. A `bool` field's getter falls back to
///   the bean-style accessor methods `get<Name>()` then `is<Name>()` when
///   the field does not exist (mirroring the `bean` module's read path).
/// * `static field NAME: Type;` — a Java static field: generated as
///   `pub fn get_static_<name>(java: &rjava::Java) -> rjava::JavaResult<Type>`
///   and `pub fn set_static_<name>(java: &rjava::Java, v: Type) ->
///   rjava::JavaResult<()>` over the static field.
///
/// # Type mapping (macro time)
///
/// The JNI descriptor of every method is computed at **compile time** from
/// the declared Rust types (mirroring `rjava`'s `ToJava`/`FromJava`
/// machinery):
///
/// | Rust | JNI |
/// |------|-----|
/// | `()` | `V` (return only) |
/// | `bool` | `Z` |
/// | `i8` / `u8` | `B` |
/// | `i16` | `S` |
/// | `i32` | `I` |
/// | `i64` | `J` |
/// | `f32` | `F` |
/// | `f64` | `D` |
/// | `char` | `C` |
/// | `String` / `&str` | `Ljava/lang/String;` |
/// | `JObject` | `Ljava/lang/Object;` (generic — the exact signature is resolved at runtime via the reflection fallback) |
/// | `Option<T>` | the descriptor of `T` (null-tolerant, mirroring `FromJava`/`ToJava`) |
/// | `Vec<T>` | `[` + the descriptor of `T` (`Vec<String>` → `[Ljava/lang/String;`, `Vec<u8>` → `[B`) |
/// | `Self` | `L` + the class name + `;` (return only) |
///
/// # What the expansion contains
///
/// The wrapper struct, one typed method per declaration (all referencing
/// `::rjava::…`), and a [`rjava::bind::JavaBound`](https://docs.rs/rjava/latest/rjava/bind/trait.JavaBound.html)
/// impl holding the per-wrapper class cache (a `OnceLock` over a JNI global
/// reference — `Send + Sync`, so **no `unsafe`**). Method IDs are **not**
/// cached across calls (jni-rs `MethodID` carries an `Env` lifetime, so a
/// cached ID would need `unsafe`); each call re-derives the method ID on the
/// cached class. The expansion contains no `unsafe`, so
/// `#![forbid(unsafe_code)]` user crates keep compiling — like the
/// `native!`/`native_inst!` expansions.
#[proc_macro]
pub fn bind(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as BindInput);
    match expand_bind(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ---------------------------------------------------------------------------
// interface! — typed Java-interface implementation (the IMPLEMENT side)
// ---------------------------------------------------------------------------

/// The parsed input of `interface!`: `"class.Name" => Trait { fn items }`.
///
/// Shares the [`BindInput`] parser (same grammar as `bind!` minus the forms
/// `interface!` rejects: `static fn` and `field` — with targeted error
/// messages naming the rejected form and the reason).
struct InterfaceInput {
    class: LitStr,
    name: Ident,
    methods: Vec<BindMethod>,
}

impl InterfaceInput {
    fn from_bind(input: BindInput) -> syn::Result<Self> {
        if !input.fields.is_empty() {
            let f = &input.fields[0];
            return Err(syn::Error::new(
                f.name.span(),
                "`field` declarations are not supported in interface! — an interface is \
                 implemented by trait methods on your Rust state, not by fields; bind fields \
                 with bind! instead",
            ));
        }
        Ok(InterfaceInput {
            class: input.class,
            name: input.name,
            methods: input.methods,
        })
    }
}

/// The binary name (`Class.getName()` form) of a **declared parameter type**
/// — the dotted class name for references, the primitive keyword for
/// primitives, and the JNI-descriptor form for arrays (`String[]` →
/// `"[Ljava.lang.String;"`, `int[]` → `"[I"`) — matching what
/// [`Call::param_types`] carries at call time, so the generated method table
/// can be matched against the real Java signature.
fn binary_name(m: &Mapped) -> String {
    match m {
        Mapped::Prim(c) => prim_binary_name(*c).to_string(),
        Mapped::Str => "java.lang.String".to_string(),
        Mapped::Object => "java.lang.Object".to_string(),
        Mapped::SelfRef => unreachable!("Self is rejected in interface! before binary_name"),
        Mapped::OptionOf(inner) => binary_name(inner),
        Mapped::VecOf(inner) => format!("[{}", elem_binary(inner)),
    }
}

/// The `Class.getName()` fragment of a type **inside an array**: primitives
/// become their JNI letters and references the `L`-prefixed dotted name with
/// a trailing `;`, because `Class.getName()` renders array types as
/// `[` + element-name + `;` where the element name is dotted (the JVM's
/// `[Ljava.lang.String;` for `String[]`, `[I` for `int[]`).
fn elem_binary(m: &Mapped) -> String {
    match m {
        Mapped::Prim(c) => c.to_string(),
        Mapped::Str => "Ljava.lang.String;".to_string(),
        Mapped::Object => "Ljava.lang.Object;".to_string(),
        Mapped::SelfRef => unreachable!("Self is rejected in interface! before binary_name"),
        Mapped::OptionOf(inner) => elem_binary(inner),
        Mapped::VecOf(inner) => format!("[{}", elem_binary(inner)),
    }
}

/// The binary name of a primitive letter.
fn prim_binary_name(c: char) -> &'static str {
    match c {
        'Z' => "boolean",
        'B' => "byte",
        'C' => "char",
        'S' => "short",
        'I' => "int",
        'J' => "long",
        'F' => "float",
        'D' => "double",
        'V' => "void",
        _ => unreachable!("only primitive letters reach prim_binary_name"),
    }
}

/// Validate an `interface!` parameter mapping: the dispatch must be able to
/// **own** the converted value (`FromJava`), so `&str` (which has no
/// `FromJava`) and `Self` (there is no wrapper to re-wrap) are rejected.
fn validate_interface_param(m: &Mapped, ty: &Type) -> syn::Result<()> {
    match m {
        Mapped::SelfRef => Err(syn::Error::new(
            ty.span(),
            "`Self` is not supported in interface! — the implemented object is your Rust \
             state, not a Java wrapper; use a concrete type (e.g. `JObject`)",
        )),
        Mapped::Prim('V') => Err(syn::Error::new(
            ty.span(),
            "`()` (void) is only valid as a return type in interface!",
        )),
        _ => {
            if let Type::Reference(_) = ty {
                return Err(syn::Error::new(
                    ty.span(),
                    "`&str` cannot be an interface! parameter — the dispatch converts the JNI \
                     value into an owned Rust value; use `String`",
                ));
            }
            if let Mapped::OptionOf(inner) | Mapped::VecOf(inner) = m
                && contains_self(inner)
            {
                return Err(syn::Error::new(
                    ty.span(),
                    "`Option<Self>` / `Vec<Self>` are not supported in interface!",
                ));
            }
            Ok(())
        }
    }
}

/// Validate an `interface!` return mapping: `()` is the void return, `&str`
/// has no owned conversion, and `Self` has no wrapper to produce.
fn validate_interface_return(m: &Mapped, ty: &Type) -> syn::Result<()> {
    if let Type::Reference(_) = ty {
        return Err(syn::Error::new(
            ty.span(),
            "`&str` cannot be an interface! return type — use `String`",
        ));
    }
    if matches!(m, Mapped::SelfRef) {
        return Err(syn::Error::new(
            ty.span(),
            "`Self` is not supported in interface! — return a concrete type (e.g. `JObject`)",
        ));
    }
    if let Mapped::OptionOf(inner) | Mapped::VecOf(inner) = m
        && contains_self(inner)
    {
        return Err(syn::Error::new(
            ty.span(),
            "`Option<Self>` / `Vec<Self>` are not supported in interface!",
        ));
    }
    Ok(())
}

/// Is this (Java name, parameter binary names) pair one of the three
/// **library-reserved** `Object` methods the runtime intercepts before the
/// handler — `toString()`, `hashCode()`, `equals(Object)` — by exact
/// signature? Declaring one of these in `interface!` would generate a trait
/// method that can never be called (the interception runs first), so the
/// macro rejects it with a pointer to the reason.
fn is_reserved_object_method(java_name: &str, param_bins: &[String]) -> bool {
    match java_name {
        "toString" | "hashCode" => param_bins.is_empty(),
        "equals" => param_bins.len() == 1 && param_bins[0] == "java.lang.Object",
        _ => false,
    }
}

/// The full expansion of an `interface!` invocation: the generated trait, and
/// the hidden [`InterfaceTrait`] impl on the trait object
/// (`dyn Trait + Send + Sync`) carrying the interface's binary name and the
/// generated `dispatch` — a `match` over `(name, param_types)` that converts
/// the arguments per the declared types and calls the trait methods on
/// `self`. Every path references `::rjava::…`, so the expansion is
/// `unsafe`-free.
fn expand_interface(input: &InterfaceInput) -> syn::Result<TokenStream2> {
    let class_dotted = input.class.value();
    if class_dotted.is_empty() {
        return Err(syn::Error::new(
            input.class.span(),
            "the interface name must not be empty",
        ));
    }
    let dotted = if class_dotted.contains('/') {
        class_dotted.replace('/', ".")
    } else {
        class_dotted.clone()
    };
    let dotted_lit = LitStr::new(&dotted, input.class.span());
    let trait_name = &input.name;
    let trait_doc = format!(
        "The Rust-side implementation contract for the Java interface `{dotted}`: \
         implement every method with `fn name(&self, env: &mut jni::Env, …) -> JavaResult<R>` \
         and build a proxy with `rjava::interface::proxy_typed(state, &[\"{dotted}\"])`."
    );

    let mut trait_items = Vec::new();
    let mut dispatch_arms = Vec::new();
    let mut seen = HashSet::new();
    for m in &input.methods {
        let name = &m.name;
        let mname = name.to_string();
        if m.is_static {
            return Err(syn::Error::new(
                m.name.span(),
                "`static fn` is not supported in interface! v1 — Java static interface methods \
                 cannot be trait items (the generated trait has a `&self` receiver); keep them \
                 on the generic `rjava::interface::proxy` handler path",
            ));
        }
        if !seen.insert(mname.clone()) {
            return Err(syn::Error::new(
                m.name.span(),
                format!(
                    "duplicate method `{mname}` in interface! — Rust traits cannot declare \
                     overloads; declare each Java overload once and dispatch both from the \
                     generic `rjava::interface::proxy` handler if you need both"
                ),
            ));
        }
        let java_name = m
            .java_name
            .as_ref()
            .map(|l| l.value())
            .unwrap_or_else(|| mname.clone());
        if java_name.is_empty() {
            return Err(syn::Error::new(
                m.name.span(),
                "`java_name = \"…\"` must not be empty",
            ));
        }

        let mut param_bins = Vec::new();
        let mut param_decls = Vec::new();
        let mut param_names = Vec::new();
        let mut param_conv = Vec::new();
        for (pname, pty) in &m.params {
            let pname_str = pname.to_string();
            if pname_str == "env" {
                return Err(syn::Error::new(
                    pname.span(),
                    "`env` is reserved as the JNI `Env` parameter of every generated trait \
                     method in interface! — rename this parameter",
                ));
            }
            if pname_str.starts_with("__rjava") || matches!(pname_str.as_str(), "__env" | "__state" | "__args" | "__out") {
                return Err(syn::Error::new(
                    pname.span(),
                    format!("parameter name `{pname_str}` collides with the generated dispatch plumbing in interface! — rename it"),
                ));
            }
            let mapped = map_type(pty)?;
            validate_interface_param(&mapped, pty)?;
            param_bins.push(binary_name(&mapped));
            param_decls.push(quote! { #pname: #pty });
            param_names.push(pname);
            param_conv.push(quote! {
                let #pname = <#pty as ::rjava::FromJava>::from_java(
                    env,
                    __args.next().expect(
                        "rjava::interface: dispatch arity is guaranteed by the param_matches guard",
                    ),
                )?;
            });
        }
        // An omitted return type is the interface! void sugar (`fn ping();`).
        let ret = match &m.ret {
            Some(r) => r.clone(),
            None => syn::parse_quote!(()),
        };
        let ret_mapped = map_type(&ret)?;
        validate_interface_return(&ret_mapped, &ret)?;
        let ret_ty = &ret;

        if is_reserved_object_method(&java_name, &param_bins) {
            let shown = if param_bins.is_empty() {
                format!("{java_name}()")
            } else {
                format!("{java_name}({})", param_bins.join(", "))
            };
            return Err(syn::Error::new(
                m.name.span(),
                format!(
                    "`{shown}` is one of the library-reserved `Object` methods that the \
                     interface runtime intercepts before the handler (identity semantics), so \
                     a trait method with this exact signature could never be called; remove it \
                     from interface! — a different signature (e.g. `toString(int)`) still \
                     reaches the handler"
                ),
            ));
        }

        let name_lit = LitStr::new(&java_name, m.java_name.as_ref().map_or(m.name.span(), |l| l.span()));
        let method_doc = if java_name == mname {
            format!("Implements the Java method `{java_name}`.")
        } else {
            format!(
                "Implements the Java method `{java_name}` (declared as `{mname}` via \
                 `[java_name = \"{java_name}\"]`)."
            )
        };

        // The trait item: `&self` receiver + the env + the declared params,
        // returning the declared Rust type (or `()` for a void method).
        trait_items.push(quote! {
            #[doc = #method_doc]
            fn #name(&self, env: &mut ::rjava::jni::Env, #(#param_decls),*) -> ::rjava::JavaResult<#ret_ty>;
        });

        // The dispatch arm: the guard re-checks the arriving parameter types
        // against the declared binary names (via the shared
        // `param_matches`, which also gives a declared `JObject` parameter
        // its any-reference wildcard). The body converts the (unboxed,
        // order-preserving) arguments with the declared types' `FromJava`
        // impls — the no-widening guarantee: the guard already required the
        // exact Java parameter types, so a declared `i32` parameter receives
        // `JValueOwned::Int`, never a widened `Long` — calls the trait
        // method on `self` (the trait object), and converts the return via
        // `ToJava` + `box_value` (void → null).
        let bin_lits: Vec<LitStr> = param_bins
            .iter()
            .map(|b| LitStr::new(b, m.name.span()))
            .collect();
        dispatch_arms.push(quote! {
            #name_lit if ::rjava::interface::param_matches(&[#(#bin_lits),*], param_types) => {
                #(#param_conv)*
                let __out = self.#name(env, #(#param_names),*)?;
                let __value = ::rjava::interface::to_value(env, __out)?;
                ::std::result::Result::Ok(::rjava::interface::box_value(env, __value)?)
            }
        });
    }

    Ok(quote! {
        #[allow(non_snake_case)]
        #[doc = #trait_doc]
        pub trait #trait_name {
            #(#trait_items)*
        }

        #[doc(hidden)]
        impl ::rjava::interface::InterfaceTrait for dyn #trait_name + Send + Sync {
            const INTERFACE_NAME: &'static str = #dotted_lit;
            fn dispatch<'env>(
                &self,
                env: &mut ::rjava::jni::Env<'env>,
                name: &str,
                param_types: &[String],
                args: ::std::vec::Vec<::rjava::jni::JValueOwned<'env>>,
            ) -> ::rjava::JavaResult<::rjava::JObject> {
                let mut __args = args.into_iter();
                match name {
                    #(#dispatch_arms)*
                    _ => Err(::rjava::JavaError::JavaException {
                        class: "java.lang.RuntimeException".to_string(),
                        message: format!(
                            "rjava::interface: no method `{name}` with parameter types \
                             {param_types:?} is declared in interface! `{}` — declare it \
                             in the interface! trait, or implement the dynamic dispatch \
                             with `interface::proxy`",
                            #dotted_lit
                        ),
                    }),
                }
            }
        }
    })
}

/// Implement a Java interface from Rust, **declared once with Rust types**
/// — the typed mirror of [`bind!`] for the implement side. The macro
/// generates a Rust `trait`; you implement the trait for your state struct,
/// and `rjava::interface::proxy_typed` builds the generic handler behind the
/// scenes — no manual `JValue` conversion, no name-match dispatch.
///
/// ```ignore
/// rjava::interface! {
///     "com.example.Greeter" => Greeter {       // generates: pub trait Greeter
///         fn greet(name: String) -> String;
///         fn add(a: i32, b: i32) -> i32;
///         fn ping();                            // void
///         fn apply(words: Vec<String>) -> Option<String>;
///     }
/// }
///
/// struct MyGreeter { prefix: String }
/// impl Greeter for MyGreeter {
///     fn greet(&self, env: &mut jni::Env, name: String) -> JavaResult<String> {
///         Ok(format!("{}{name}", self.prefix))
///     }
///     // ... etc; void -> JavaResult<()>
/// }
///
/// let proxy: JObject = rjava::interface::proxy_typed(MyGreeter { prefix: "Hi, ".into() }, &["com.example.Greeter"])?;
/// ```
///
/// * `"com.example.Greeter"` — the Java interface name, dotted or slash form.
/// * `Greeter` — the name of the generated `pub trait`. Implement it for
///   your state struct; every declared method takes `&self`, the
///   [jni `Env`](https://docs.rs/jni/latest/jni/struct.Env.html), the
///   declared parameter types, and returns
///   `rjava::JavaResult<R>` (`()` for a `void` Java method).
/// * `fn name(T1, T2) -> R;` — one Java interface method. The JNI
///   conversion, dispatch and boxing are generated; a `Rust`-side
///   `JavaResult::Err` is thrown into the calling Java code exactly like the
///   generic handler's errors.
/// * `fn to_string() -> String [java_name = "toString"];` — the optional
///   Java-name alias: the trait method is `to_string`, the Java method is
///   `toString` (default: the Java name is the Rust name verbatim).
/// * **No `static fn` in v1**: static interface methods cannot be trait
///   items (the generated trait has a `&self` receiver); keep them on the
///   generic [`rjava::interface::proxy`](https://docs.rs/rjava/latest/rjava/interface/fn.proxy.html)
///   handler path.
///
/// # Type mapping (macro time)
///
/// The same mapping as `bind!` (minus `Self` and `&str`, which need an
/// owned conversion the dispatch cannot provide): `bool`, `i8`/`u8`, `i16`,
/// `i32`, `i64`, `f32`, `f64`, `char`, `String`, `JObject`, `Option<T>` and
/// `Vec<T>`. The generated method table carries the declared parameter types
/// as binary names; a call whose `(name, param_types)` matches **no**
/// declared method is a clear error. A declared `JObject` parameter matches
/// any reference parameter type (like `bind!`'s generic `JObject`); every
/// other declared parameter type must match exactly, so a declared `i32`
/// never unboxes a widened `Long`.
///
/// # What the expansion contains
///
/// The `pub trait`, and a hidden [`rjava::interface::InterfaceTrait`](https://docs.rs/rjava/latest/rjava/interface/trait.InterfaceTrait.html)
/// impl **on the trait object** (`dyn Greeter + Send + Sync` — a local self
/// type, so the impl satisfies the orphan rule and one state struct can
/// implement any number of interfaces): the interface's binary name plus a
/// generated `dispatch` whose `match` arms carry the declared Java name and
/// parameter types (the method table) and call the trait methods on `self`
/// with the converted arguments. The expansion references only `::rjava::…`
/// and contains **no `unsafe`**, so `#![forbid(unsafe_code)]` user crates
/// keep compiling.
#[proc_macro]
pub fn interface(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as BindInput);
    let input = match InterfaceInput::from_bind(input) {
        Ok(i) => i,
        Err(e) => return e.to_compile_error().into(),
    };
    match expand_interface(&input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

// ---------------------------------------------------------------------------
// Signature parsing (hand-written, not syn)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `V` — return only.
    Void,
    /// One of `Z B C S I J F D`.
    Prim(char),
    /// `L<name>;` or `[<type>]` — the C parameter is a `JObject` (ABI-safe
    /// over `jobject`), the C return value is a raw `jobject`.
    Ref,
}

struct ParsedType {
    kind: Kind,
    /// The normalized JNI fragment (dots already converted to slashes).
    frag: String,
}

fn parse_type(chars: &[char], i: &mut usize, allow_void: bool) -> Result<ParsedType, String> {
    if *i >= chars.len() {
        return Err("unexpected end of signature".to_string());
    }
    match chars[*i] {
        c @ ('Z' | 'B' | 'C' | 'S' | 'I' | 'J' | 'F' | 'D') => {
            *i += 1;
            Ok(ParsedType {
                kind: Kind::Prim(c),
                frag: c.to_string(),
            })
        }
        'V' if allow_void => {
            *i += 1;
            Ok(ParsedType {
                kind: Kind::Void,
                frag: "V".to_string(),
            })
        }
        'V' => Err("`V` is only valid as a return type, not as a parameter".to_string()),
        'L' => {
            let start = *i + 1;
            let mut j = start;
            while j < chars.len() && chars[j] != ';' {
                j += 1;
            }
            if j >= chars.len() {
                return Err("unterminated class name in `L<name>;` (missing `;`)".to_string());
            }
            let name: String = chars[start..j].iter().collect();
            if name.is_empty() {
                return Err("empty class name in `L<name>;`".to_string());
            }
            if !name.chars().all(|ch| {
                ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '$' | '_')
            }) {
                return Err(format!(
                    "invalid character in class name `{name}` (expected letters, digits, `/`, `.`, `$` or `_`)"
                ));
            }
            *i = j + 1;
            Ok(ParsedType {
                kind: Kind::Ref,
                frag: format!("L{};", name.replace('.', "/")),
            })
        }
        '[' => {
            *i += 1;
            if *i >= chars.len() {
                return Err("unterminated array type (missing element type)".to_string());
            }
            let frag = match chars[*i] {
                c @ ('Z' | 'B' | 'C' | 'S' | 'I' | 'J' | 'F' | 'D') => {
                    *i += 1;
                    format!("[{c}")
                }
                'L' => {
                    let start = *i + 1;
                    let mut j = start;
                    while j < chars.len() && chars[j] != ';' {
                        j += 1;
                    }
                    if j >= chars.len() {
                        return Err(
                            "unterminated array element class name (missing `;`)".to_string(),
                        );
                    }
                    let name: String = chars[start..j].iter().collect();
                    if name.is_empty() {
                        return Err("empty array element class name".to_string());
                    }
                    *i = j + 1;
                    format!("[L{};", name.replace('.', "/"))
                }
                other => {
                    return Err(format!(
                        "bad array element type `{other}` (expected a primitive letter or `L<name>;`)"
                    ))
                }
            };
            Ok(ParsedType {
                kind: Kind::Ref,
                frag,
            })
        }
        other => Err(format!(
            "unknown JNI type `{other}` (expected Z B C S I J F D, `L<name>;`, `[<type>]`, or `V` as a return type)"
        )),
    }
}

/// Parse a JNI method descriptor `(<params>)<ret>`.
///
/// The JVM caps a method descriptor at 255 parameter units (JVMS §4.3.3):
/// every parameter occupies one unit, except `long` and `double`, which
/// occupy two. An instance method's implicit `this` receiver consumes one of
/// the 255, so `is_inst` methods may declare at most 254 units of parameters;
/// static methods may declare at most 255.
fn parse_sig(sig: &str, is_inst: bool) -> Result<(Vec<ParsedType>, ParsedType), String> {
    let chars: Vec<char> = sig.chars().collect();
    if chars.first() != Some(&'(') {
        return Err("signature must start with `(` — e.g. \"(II)I\"".to_string());
    }
    let max_units = if is_inst { 254 } else { 255 };
    let mut i = 1;
    let mut params = Vec::new();
    loop {
        if i >= chars.len() {
            return Err("unterminated parameter list — missing `)`".to_string());
        }
        if chars[i] == ')' {
            i += 1;
            break;
        }
        params.push(parse_type(&chars, &mut i, false)?);
    }
    let units = params
        .iter()
        .map(|p| match p.kind {
            Kind::Prim('J' | 'D') => 2,
            _ => 1,
        })
        .sum::<usize>();
    if units > max_units {
        let receiver = if is_inst {
            " (the instance receiver `this` takes 1 of the 255, leaving 254)"
        } else {
            ""
        };
        return Err(format!(
            "too many parameters: the JVM method-descriptor limit is {max_units} parameter \
             units (JVMS §4.3.3; `long`/`double` count as 2 units{receiver}) and this \
             signature needs {units}"
        ));
    }
    if i >= chars.len() {
        return Err("missing return type after `)`".to_string());
    }
    let ret = parse_type(&chars, &mut i, true)?;
    if i != chars.len() {
        return Err(format!(
            "unexpected trailing characters after the return type: `{}`",
            chars[i..].iter().collect::<String>()
        ));
    }
    Ok((params, ret))
}

// ---------------------------------------------------------------------------
// Codegen helpers
// ---------------------------------------------------------------------------

/// The raw C type for a primitive letter.
fn prim_ty(c: char) -> TokenStream2 {
    match c {
        'Z' => quote!(::rjava::jni::sys::jboolean),
        'B' => quote!(::rjava::jni::sys::jbyte),
        'C' => quote!(::rjava::jni::sys::jchar),
        'S' => quote!(::rjava::jni::sys::jshort),
        'I' => quote!(::rjava::jni::sys::jint),
        'J' => quote!(::rjava::jni::sys::jlong),
        'F' => quote!(::rjava::jni::sys::jfloat),
        'D' => quote!(::rjava::jni::sys::jdouble),
        _ => unreachable!("only primitive letters reach prim_ty"),
    }
}

/// The `JavaArg` variant for a primitive letter.
fn prim_arg(c: char) -> TokenStream2 {
    let ident = format_ident!("{}", match c {
        'Z' => "Bool",
        'B' => "Byte",
        'C' => "Char",
        'S' => "Short",
        'I' => "Int",
        'J' => "Long",
        'F' => "Float",
        'D' => "Double",
        _ => unreachable!("only primitive letters reach prim_arg"),
    });
    quote!(#ident)
}

/// The `JValueOwned` variant for a primitive letter.
fn prim_variant(c: char) -> TokenStream2 {
    match c {
        'Z' => quote!(Bool),
        'B' => quote!(Byte),
        'C' => quote!(Char),
        'S' => quote!(Short),
        'I' => quote!(Int),
        'J' => quote!(Long),
        'F' => quote!(Float),
        'D' => quote!(Double),
        _ => unreachable!("only primitive letters reach prim_variant"),
    }
}

/// The default C value for a primitive letter.
fn prim_default(c: char) -> TokenStream2 {
    match c {
        'Z' => quote!(false),
        'B' | 'C' | 'S' | 'I' | 'J' => quote!(0),
        'F' | 'D' => quote!(0.0),
        _ => unreachable!("only primitive letters reach prim_default"),
    }
}

/// The C return type for a parsed return type.
fn ret_ty(ret: &ParsedType) -> TokenStream2 {
    match ret.kind {
        Kind::Void => quote!(()),
        Kind::Prim(c) => prim_ty(c),
        Kind::Ref => quote!(::rjava::jni::sys::jobject),
    }
}

/// The default C value for a parsed return type.
fn ret_default(ret: &ParsedType) -> TokenStream2 {
    match ret.kind {
        Kind::Void => quote!(()),
        Kind::Prim(c) => prim_default(c),
        Kind::Ref => quote!(::std::ptr::null_mut()),
    }
}

/// Convert the dispatched `JValueOwned` into the C return type; a mismatched
/// variant throws `InvalidArgument` and returns the default.
fn ret_conv(env: &TokenStream2, v: &TokenStream2, ret: &ParsedType) -> TokenStream2 {
    let msg = "native method returned a Java value that does not match its JNI signature";
    match ret.kind {
        Kind::Void => quote! {
            match #v {
                ::rjava::jni::JValueOwned::Void => (),
                _ => {
                    let _ = ::rjava::native::throw_error(
                        #env,
                        ::rjava::JavaError::InvalidArgument(#msg),
                    );
                    ()
                }
            }
        },
        Kind::Prim(c) => {
            let var = prim_variant(c);
            let def = prim_default(c);
            quote! {
                match #v {
                    ::rjava::jni::JValueOwned::#var(__vv) => __vv,
                    _ => {
                        let _ = ::rjava::native::throw_error(
                            #env,
                            ::rjava::JavaError::InvalidArgument(#msg),
                        );
                        #def
                    }
                }
            }
        }
        Kind::Ref => quote! {
            match #v {
                ::rjava::jni::JValueOwned::Object(__vv) => __vv.into_raw(),
                _ => {
                    let _ = ::rjava::native::throw_error(
                        #env,
                        ::rjava::JavaError::InvalidArgument(#msg),
                    );
                    ::std::ptr::null_mut()
                }
            }
        },
    }
}

// ---------------------------------------------------------------------------
// Expansion
// ---------------------------------------------------------------------------

fn expand(input: &Input, is_inst: bool) -> TokenStream2 {
    let macro_name = if is_inst { "native_inst" } else { "native" };
    let sig_lit = input.sig.as_ref().expect("expand is only called for the explicit form");
    let sig_str = sig_lit.value();
    let (params, ret) = match parse_sig(&sig_str, is_inst) {
        Ok(parsed) => parsed,
        Err(msg) => {
            return syn::Error::new(
                sig_lit.span(),
                format!(
                    "rjava::{macro_name}! invalid JNI signature `{sig_str}`: {msg}; \
                     expected e.g. \"(II)I\", \"(Ljava/lang/String;)V\" or \"([D)D\"",
                ),
            )
            .to_compile_error();
        }
    };

    // The JVM hands every native method a raw `JNIEnv*`; the JNI *type* of
    // each following parameter is derived from the signature letter.
    let receiver_decl = if is_inst {
        quote! { receiver: ::rjava::jni::objects::JObject<'caller>, }
    } else {
        quote! { _receiver: ::rjava::jni::objects::JClass<'caller>, }
    };
    let mut decls = Vec::new();
    let mut pushes = Vec::new();
    for (idx, p) in params.iter().enumerate() {
        let n = format_ident!("n{idx}");
        match p.kind {
            Kind::Prim(c) => {
                let ty = prim_ty(c);
                let arg = prim_arg(c);
                decls.push(quote! { #n: #ty, });
                pushes.push(quote! { __args.push(::rjava::JavaArg::#arg(#n)); });
            }
            // `L<name>;` and `[<type>]` parameters arrive as object
            // references; jni's `JObject` is `repr(transparent)` over
            // `jobject`, so it is ABI-safe as the C parameter type, and
            // `JavaArg::Object` carries it as-is.
            Kind::Ref => {
                decls.push(quote! { #n: ::rjava::jni::objects::JObject<'caller>, });
                pushes.push(quote! { __args.push(::rjava::JavaArg::Object(#n)); });
            }
            Kind::Void => unreachable!("V is rejected as a parameter by the parser"),
        }
    }

    let name = &input.name;
    let sig_lit = LitStr::new(&format!("({}){}", params.iter().map(|p| p.frag.clone()).collect::<String>(), ret.frag), sig_lit.span());
    let f = &input.f;
    let ret_ty = ret_ty(&ret);
    let default = ret_default(&ret);
    let conv = ret_conv(&quote!(__env), &quote!(__v), &ret);

    let receiver_push = if is_inst {
        quote! { __args.push(::rjava::JavaArg::Object(receiver)); }
    } else {
        quote! {}
    };

    quote! {
        (|| {
            #[allow(unused_parens, clippy::unused_unit)]
            extern "system" fn __rjava_tramp<'caller>(
                mut __unowned_env: ::rjava::jni::EnvUnowned<'caller>,
                #receiver_decl
                #(#decls)*
            ) -> #ret_ty {
                // The JVM guarantees `__unowned_env` is a valid, non-null
                // `JNIEnv*` for the current thread for the duration of the
                // call, and that the local-reference frame it belongs to is
                // only popped after this function returns. `as_raw` is a safe
                // getter; it is kept here as documentation of that contract.
                let __raw = __unowned_env.as_raw();
                match __unowned_env.with_env(
                    |__env| -> ::std::result::Result<#ret_ty, ::rjava::jni::errors::Error> {
                        let mut __args: ::std::vec::Vec<::rjava::JavaArg<'caller>> =
                            ::std::vec::Vec::new();
                        #receiver_push
                        #(#pushes)*
                        match ::rjava::native::dispatch(__env, __args, #f) {
                            ::std::result::Result::Ok(__v) => {
                                ::std::result::Result::Ok(#conv)
                            }
                            ::std::result::Result::Err(__e) => {
                                let _ = ::rjava::native::throw_error(__env, __e);
                                ::std::result::Result::Ok(#default)
                            }
                        }
                    },
                ).into_outcome() {
                    ::rjava::jni::Outcome::Ok(__out) => __out,
                    // The non-`Ok` arms (its own catch_unwind already covers
                    // dispatch's panics) are rare; returning the signature's
                    // default value is acceptable — the JVM simply observes a
                    // default return value.
                    _ => #default,
                }
            }
            ::rjava::NativeMethod::new(
                #name,
                #sig_lit,
                __rjava_tramp as *mut ::std::ffi::c_void,
            )
        })()
    }
}

/// The **type-derived** expansion: no signature string, no per-call
/// trampoline. `make_native_*` picks a shared generic trampoline from the
/// Rust types at compile time, derives the JNI signature at runtime, and
/// stores the user's callable in a process-global registry keyed by the
/// trampoline's own address.
///
/// The expansion is a single plain-safe call — no `unsafe`, no generated
/// items — so `#![forbid(unsafe_code)]` user crates (and rjava's own
/// integration tests) keep compiling.
fn expand_derived(input: &Input, is_inst: bool) -> TokenStream2 {
    let name = &input.name;
    let f = &input.f;
    let make = if is_inst {
        quote!(::rjava::native::make_native_inst)
    } else {
        quote!(::rjava::native::make_native_static)
    };
    quote! {
        (|| #make(#name, #f))()
    }
}

/// The `async_native!` expansion: like [`expand_derived`], a single plain-safe
/// call — `A` (the argument tuple) and `R` (the future's value type) are
/// inferred from `f`'s signature, and `make_async_native` picks the shared
/// per-`(A)` async trampoline, derives the JNI parameter descriptor at
/// runtime, and stores the callable in the process-global registry keyed by
/// the trampoline's address (same collision detection as the sync path).
fn expand_async_derived(input: &AsyncInput) -> TokenStream2 {
    let name = &input.name;
    let f = &input.f;
    quote! {
        (|| ::rjava::future::make_async_native(#name, #f))()
    }
}

#[cfg(test)]
mod tests {
    use super::parse_sig;

    /// `(<c>×n)V` — a signature with `n` parameters of one primitive letter.
    fn repeat(c: char, n: usize) -> String {
        format!("({})V", std::iter::repeat(c).take(n).collect::<String>())
    }

    #[test]
    fn static_limit_is_255_units() {
        // 255 ints = 255 units — exactly at the static cap (JVMS §4.3.3).
        let (params, _ret) = parse_sig(&repeat('I', 255), false).expect("255 ints parse for static");
        assert_eq!(params.len(), 255);
        // 256 ints exceed it.
        assert!(parse_sig(&repeat('I', 256), false).is_err());
        // `long`/`double` count as two units: 127 longs (254) + 1 int = 255.
        assert!(parse_sig(&format!("({}I)V", "J".repeat(127)), false).is_ok());
        // 128 longs = 256 units.
        assert!(parse_sig(&repeat('J', 128), false).is_err());
    }

    #[test]
    fn instance_limit_is_254_units() {
        // The instance receiver `this` takes one of the 255 units.
        assert!(parse_sig(&repeat('I', 254), true).is_ok());
        assert!(parse_sig(&repeat('I', 255), true).is_err());
        // 127 longs = 254 units — exactly the instance cap.
        assert!(parse_sig(&repeat('J', 127), true).is_ok());
        assert!(parse_sig(&repeat('J', 128), true).is_err());
    }

    #[test]
    fn error_message_names_the_jvm_limit() {
        let err = parse_sig(&repeat('I', 256), false)
            .err()
            .expect("256 ints must be rejected for static");
        assert!(err.contains("255"), "message: {err}");
        let err = parse_sig(&repeat('I', 255), true)
            .err()
            .expect("255 ints must be rejected for instance");
        assert!(err.contains("254"), "message: {err}");
        assert!(err.contains("this"), "message: {err}");
    }

    #[test]
    fn many_parameter_signatures_parse() {
        // A 20-int static signature — the shape of the fixture's `many20`.
        let (params, _ret) = parse_sig("(IIIIIIIIIIIIIIIIIIII)J", false).expect("20 ints parse");
        assert_eq!(params.len(), 20);
        // Mixed types incl. `long`/`double` — unit accounting stays exact
        // (13 units here): B S I J F D Z C L…; J.
        let (params, _ret) = parse_sig("(BSIJFDZCLjava/lang/String;J)J", false)
            .expect("mixed signature parses");
        assert_eq!(params.len(), 10);
    }
}

#[cfg(test)]
mod async_native_tests {
    use super::{expand_async_derived, AsyncInput};
    use quote::quote;

    /// Parse an `async_native!` invocation and expand it.
    fn expand(tokens: proc_macro2::TokenStream) -> String {
        let input: AsyncInput = syn::parse2(tokens).expect("async_native input parses");
        expand_async_derived(&input).to_string()
    }

    #[test]
    fn expansion_calls_make_async_native() {
        // fn item form.
        let out = expand(quote! {
            "compute", compute
        });
        assert!(
            out.contains(":: rjava :: future :: make_async_native (\"compute\" , compute)"),
            "expansion: {out}"
        );
        // Closure form — parsed as an expression, passed through verbatim.
        let out = expand(quote! {
            "compute",
            |java: rjava::Java, (a, b): (i32, i32)| async move { Ok(a + b) }
        });
        assert!(
            out.contains("make_async_native (\"compute\" , | java : rjava :: Java , (a , b) : (i32 , i32) | async move { Ok (a + b) })"),
            "expansion: {out}"
        );
        // No signature string is accepted in v1.
        let err = syn::parse2::<AsyncInput>(quote! {
            "compute", "(II)Ljava/util/concurrent/CompletableFuture;", compute
        });
        assert!(err.is_err(), "the explicit-signature form is not available for async_native!");
    }

    #[test]
    fn trailing_tokens_are_rejected() {
        let err = syn::parse2::<AsyncInput>(quote! {
            "compute", compute, extra
        });
        assert!(err.is_err());
    }
}

#[cfg(test)]
mod bind_tests {
    use super::{contains_self, expand_bind, frag, map_type};
    use quote::quote;
    use syn::Type;

    fn type_of(ts: proc_macro2::TokenStream) -> Type {
        syn::parse2(ts).expect("test type parses")
    }

    #[test]
    fn primitives_map_to_letters() {
        assert_eq!(frag(&map_type(&type_of(quote!(bool))).unwrap(), "X"), "Z");
        assert_eq!(frag(&map_type(&type_of(quote!(i8))).unwrap(), "X"), "B");
        assert_eq!(frag(&map_type(&type_of(quote!(u8))).unwrap(), "X"), "B");
        assert_eq!(frag(&map_type(&type_of(quote!(i16))).unwrap(), "X"), "S");
        assert_eq!(frag(&map_type(&type_of(quote!(i32))).unwrap(), "X"), "I");
        assert_eq!(frag(&map_type(&type_of(quote!(i64))).unwrap(), "X"), "J");
        assert_eq!(frag(&map_type(&type_of(quote!(f32))).unwrap(), "X"), "F");
        assert_eq!(frag(&map_type(&type_of(quote!(f64))).unwrap(), "X"), "D");
        assert_eq!(frag(&map_type(&type_of(quote!(char))).unwrap(), "X"), "C");
        assert_eq!(frag(&map_type(&type_of(quote!(()))).unwrap(), "X"), "V");
    }

    #[test]
    fn references_and_generics_map() {
        let s = frag(&map_type(&type_of(quote!(String))).unwrap(), "X");
        assert_eq!(s, "Ljava/lang/String;");
        let s = frag(&map_type(&type_of(quote!(&str))).unwrap(), "X");
        assert_eq!(s, "Ljava/lang/String;");
        let s = frag(&map_type(&type_of(quote!(JObject))).unwrap(), "X");
        assert_eq!(s, "Ljava/lang/Object;");
        // Option<T> and Vec<T> unwrap to the inner descriptor.
        let s = frag(&map_type(&type_of(quote!(Option<String>))).unwrap(), "X");
        assert_eq!(s, "Ljava/lang/String;");
        let s = frag(&map_type(&type_of(quote!(Vec<String>))).unwrap(), "X");
        assert_eq!(s, "[Ljava/lang/String;");
        let s = frag(&map_type(&type_of(quote!(Vec<u8>))).unwrap(), "X");
        assert_eq!(s, "[B");
        let s = frag(&map_type(&type_of(quote!(Vec<Option<String>>))).unwrap(), "X");
        assert_eq!(s, "[Ljava/lang/String;");
        let s = frag(&map_type(&type_of(quote!(Option<Vec<i32>>))).unwrap(), "X");
        assert_eq!(s, "[I");
        // Self uses the bound class descriptor.
        let s = frag(&map_type(&type_of(quote!(Self))).unwrap(), "java/lang/StringBuilder");
        assert_eq!(s, "Ljava/lang/StringBuilder;");
    }

    #[test]
    fn unsupported_types_are_rejected() {
        assert!(map_type(&type_of(quote!(u32))).is_err(), "u32 has no Java type");
        assert!(map_type(&type_of(quote!(u16))).is_err(), "u16 has no Java type");
        assert!(map_type(&type_of(quote!(JClass))).is_err(), "JClass stays on the dynamic path");
        assert!(map_type(&type_of(quote!(JArray<i32>))).is_err(), "JArray stays on the dynamic path");
        assert!(map_type(&type_of(quote!(&String))).is_err(), "only &str is supported");
        assert!(map_type(&type_of(quote!(Option))).is_err(), "Option needs a type argument");
        assert!(map_type(&type_of(quote!(Vec<u32>))).is_err(), "Vec<u32> has no Java element type");
    }

    #[test]
    fn expansion_computes_descriptors() {
        let input: super::BindInput = syn::parse2(quote! {
            "java.lang.StringBuilder" => StringBuilder {
                fn append(s: &str) -> Self;
                fn length() -> i32;
                fn toString() -> String;
                static fn valueOf(b: bool) -> String;
            }
        })
        .expect("bind input parses");
        let ts = expand_bind(&input).expect("bind input expands");
        let out = ts.to_string();
        // The macro-time descriptors are embedded as jni_sig! literals.
        assert!(out.contains(r#"(Ljava/lang/String;)Ljava/lang/StringBuilder;"#), "{out}");
        assert!(out.contains(r#"()I"#), "{out}");
        assert!(out.contains(r#"()Ljava/lang/String;"#), "{out}");
        assert!(out.contains(r#"(Z)Ljava/lang/String;"#), "{out}");
        // The wrapper struct, the typed methods and the JavaBound impl exist
        // (token rendering adds spaces, so match on stable substrings).
        assert!(out.contains("pub struct StringBuilder"), "{out}");
        assert!(out.contains("fn append"), "{out}");
        assert!(out.contains("s : & str"), "{out}");
        assert!(out.contains("fn valueOf"), "{out}");
        assert!(out.contains("java : & :: rjava :: Java"), "{out}");
        assert!(out.contains("impl :: rjava :: bind :: JavaBound for StringBuilder"), "{out}");
        assert!(out.contains("instance_self"), "{out}");
        assert!(out.contains("instance_method"), "{out}");
        // All paths go through ::rjava:: — and no unsafe appears.
        assert!(!out.contains("unsafe"), "{out}");
    }

    #[test]
    fn duplicate_method_names_are_rejected() {
        let input: super::BindInput = syn::parse2(quote! {
            "java.lang.StringBuilder" => StringBuilder {
                fn append(s: &str) -> Self;
                fn append(n: i32) -> Self;
            }
        })
        .expect("bind input parses");
        let err = expand_bind(&input).expect_err("duplicate method names must be rejected");
        assert!(err.to_string().contains("duplicate method `append`"), "{err}");
    }

    #[test]
    fn invalid_positions_are_rejected() {
        // Self in parameter position.
        let input: super::BindInput = syn::parse2(quote! {
            "A" => A { fn f(a: Self) -> i32; }
        })
        .expect("parses");
        assert!(expand_bind(&input).is_err(), "Self as a parameter must be rejected");

        // &str as a return type.
        let input: super::BindInput = syn::parse2(quote! {
            "A" => A { fn f() -> &str; }
        })
        .expect("parses");
        let err = expand_bind(&input).expect_err("&str return must be rejected");
        assert!(err.to_string().contains("&str"), "{err}");

        // Option<Self> as a return type.
        let input: super::BindInput = syn::parse2(quote! {
            "A" => A { fn f() -> Option<Self>; }
        })
        .expect("parses");
        assert!(expand_bind(&input).is_err(), "Option<Self> must be rejected");

        // An omitted `-> Ret` parses in the shared grammar (the interface!
        // void sugar) but bind! rejects it with a clear message.
        let input: super::BindInput = syn::parse2(quote! {
            "A" => A { fn f() ; }
        })
        .expect("the shared grammar accepts an omitted return type");
        let err = expand_bind(&input).expect_err("bind! requires an explicit return type");
        assert!(err.to_string().contains("return type"), "{err}");
    }

    #[test]
    fn mapped_self_is_detected_in_generics() {
        assert!(contains_self(&map_type(&type_of(quote!(Self))).unwrap()));
        assert!(contains_self(&map_type(&type_of(quote!(Option<Self>))).unwrap()));
        assert!(!contains_self(&map_type(&type_of(quote!(Vec<String>))).unwrap()));
    }

    #[test]
    fn fields_expand_to_getter_setter_pairs() {
        let input: super::BindInput = syn::parse2(quote! {
            "bind.Kit" => Kit {
                field label: String;
                field count: i32;
                static field MAGIC: i64;
            }
        })
        .expect("bind input with fields parses");
        let ts = expand_bind(&input).expect("bind input with fields expands");
        let out = ts.to_string();
        // Instance accessors: get_<name> / set_<name> over the field name.
        assert!(out.contains("fn get_label"), "{out}");
        assert!(out.contains("fn set_label"), "{out}");
        assert!(out.contains("instance_field_get"), "{out}");
        assert!(out.contains("instance_field_set"), "{out}");
        // The Java field names are the declared idents verbatim.
        assert!(out.contains("\"label\""), "{out}");
        assert!(out.contains("\"count\""), "{out}");
        // Static accessors: get_static_<name> / set_static_<name> take the facade.
        assert!(out.contains("fn get_static_magic"), "{out}");
        assert!(out.contains("fn set_static_magic"), "{out}");
        assert!(out.contains("static_field_get"), "{out}");
        assert!(out.contains("static_field_set"), "{out}");
        assert!(out.contains("java : & :: rjava :: Java"), "{out}");
        assert!(!out.contains("unsafe"), "{out}");
    }

    #[test]
    fn bool_fields_use_the_accessor_fallback_getter() {
        let input: super::BindInput = syn::parse2(quote! {
            "bind.Kit" => Kit {
                field active: bool;
            }
        })
        .expect("parses");
        let out = expand_bind(&input).expect("expands").to_string();
        assert!(out.contains("instance_bool_field_get"), "{out}");
        assert!(out.contains("instance_field_set"), "{out}");
    }

    #[test]
    fn invalid_field_types_are_rejected() {
        for (decl, needle) in [
            ("field f: ();", "void"),
            ("field f: Self;", "Self"),
            ("field f: &str;", "&str"),
            ("field f: Option<Self>;", "Option<Self>"),
        ] {
            let input: super::BindInput =
                syn::parse_str(&format!("\"A\" => A {{ {decl} }}")).expect("parses");
            let err = expand_bind(&input).expect_err("invalid field type must be rejected");
            assert!(err.to_string().contains(needle), "{decl}: {err}");
        }
    }

    #[test]
    fn java_name_alias_reroutes_the_java_method() {
        let input: super::BindInput = syn::parse2(quote! {
            "java.lang.Object" => Obj {
                fn to_string() -> String [java_name = "toString"];
            }
        })
        .expect("alias parses");
        let out = expand_bind(&input).expect("alias expands").to_string();
        // The Rust method keeps the declared name; the Java name is the alias.
        assert!(out.contains("fn to_string"), "{out}");
        assert!(out.contains("\"toString\""), "{out}");
        assert!(out.contains("instance_method"), "{out}");
        assert!(!out.contains("unsafe"), "{out}");
    }

    #[test]
    fn duplicate_java_names_are_rejected() {
        let input: super::BindInput = syn::parse2(quote! {
            "java.lang.Object" => Obj {
                fn a() -> String [java_name = "toString"];
                fn b() -> String [java_name = "toString"];
            }
        })
        .expect("parses");
        let err = expand_bind(&input).expect_err("duplicate java_name must be rejected");
        assert!(err.to_string().contains("duplicate Java method name `toString`"), "{err}");
    }

    #[test]
    fn empty_java_name_is_rejected() {
        let input: super::BindInput = syn::parse2(quote! {
            "java.lang.Object" => Obj {
                fn a() -> String [java_name = ""];
            }
        })
        .expect("parses");
        let err = expand_bind(&input).expect_err("empty java_name must be rejected");
        assert!(err.to_string().contains("must not be empty"), "{err}");
    }

    #[test]
    fn malformed_alias_is_a_parse_error() {
        let err = syn::parse2::<super::BindInput>(quote! {
            "java.lang.Object" => Obj {
                fn a() -> String [other = "x"];
            }
        })
        .err()
        .expect("an unknown alias key must be a parse error");
        assert!(err.to_string().contains("java_name"), "{err}");
    }
}

#[cfg(test)]
mod interface_tests {
    use super::{expand_interface, InterfaceInput};
    use quote::quote;

    fn parse(input: proc_macro2::TokenStream) -> InterfaceInput {
        let bind: super::BindInput = syn::parse2(input).expect("interface input parses");
        InterfaceInput::from_bind(bind).expect("no fields in interface input")
    }

    #[test]
    fn expansion_contains_trait_dispatch_and_table() {
        let input = parse(quote! {
            "com.example.Greeter" => Greeter {
                fn greet(name: String) -> String;
                fn add(a: i32, b: i32) -> i32;
                fn ping();
            }
        });
        let out = expand_interface(&input).expect("interface input expands").to_string();
        // The generated trait with typed methods (env + declared params).
        assert!(out.contains("pub trait Greeter"), "{out}");
        assert!(out.contains("fn greet (& self , env : & mut :: rjava :: jni :: Env , name : String) -> :: rjava :: JavaResult < String > ;"), "{out}");
        assert!(out.contains("fn add (& self , env : & mut :: rjava :: jni :: Env , a : i32 , b : i32) -> :: rjava :: JavaResult < i32 > ;"), "{out}");
        assert!(out.contains("fn ping (& self , env : & mut :: rjava :: jni :: Env ,) -> :: rjava :: JavaResult < () > ;"), "{out}");
        // The hidden trait-object impl carrying the interface name and the
        // generated dispatch (the method table inlined as match arms).
        assert!(out.contains("impl :: rjava :: interface :: InterfaceTrait for dyn Greeter + Send + Sync"), "{out}");
        assert!(out.contains("const INTERFACE_NAME : & 'static str = \"com.example.Greeter\""), "{out}");
        assert!(out.contains("fn dispatch < 'env >"), "{out}");
        // The guards carry the declared parameter types as binary names.
        assert!(out.contains("param_matches (& [\"java.lang.String\"] , param_types)"), "{out}");
        assert!(out.contains("param_matches (& [\"int\" , \"int\"] , param_types)"), "{out}");
        assert!(out.contains("param_matches (& [] , param_types)"), "{out}");
        // Dispatch converts via FromJava/ToJava and boxes the result; the
        // no-match arm is a clear error naming the method.
        assert!(out.contains(":: rjava :: interface :: box_value"), "{out}");
        assert!(out.contains(":: rjava :: interface :: to_value"), "{out}");
        assert!(out.contains("no method `{name}` with parameter types"), "{out}");
        assert!(!out.contains("unsafe"), "{out}");
    }

    #[test]
    fn vec_option_and_void_map_to_binary_names() {
        let input = parse(quote! {
            "I" => I {
                fn apply(words: Vec<String>) -> Option<String>;
                fn values(v: Vec<i32>);
            }
        });
        let out = expand_interface(&input).expect("expands").to_string();
        assert!(out.contains("\"[Ljava.lang.String;\""), "{out}");
        assert!(out.contains("\"[I\""), "{out}");
        // The omitted return type is the void sugar: JavaResult<()>.
        assert!(out.contains("fn values (& self , env : & mut :: rjava :: jni :: Env , v : Vec < i32 >) -> :: rjava :: JavaResult < () > ;"), "{out}");
    }

    #[test]
    fn java_name_alias_works_in_interface() {
        let input = parse(quote! {
            "C" => C {
                fn to_string(n: i32) -> String [java_name = "toString"];
            }
        });
        let out = expand_interface(&input).expect("expands").to_string();
        assert!(out.contains("fn to_string"), "{out}");
        assert!(out.contains("\"toString\""), "{out}");
    }

    #[test]
    fn static_fns_are_rejected() {
        let input: super::BindInput = syn::parse2(quote! {
            "C" => C { static fn f() -> i32; }
        })
        .expect("parses");
        let input = InterfaceInput::from_bind(input).expect("no fields");
        let err = expand_interface(&input).expect_err("static fn must be rejected");
        assert!(err.to_string().contains("`static fn` is not supported"), "{err}");
    }

    #[test]
    fn fields_are_rejected_with_a_pointer_to_bind() {
        let input: super::BindInput = syn::parse2(quote! {
            "C" => C { field x: i32; }
        })
        .expect("parses");
        let err = match InterfaceInput::from_bind(input) {
            Ok(_) => panic!("fields must be rejected"),
            Err(e) => e,
        };
        assert!(err.to_string().contains("field"), "{err}");
        assert!(err.to_string().contains("bind!"), "{err}");
    }

    #[test]
    fn reserved_object_methods_are_rejected() {
        for decl in [
            "fn toString() -> String;",
            "fn hashCode() -> i32;",
            "fn equals(o: JObject) -> bool;",
        ] {
            let input: super::BindInput =
                syn::parse_str(&format!("\"C\" => C {{ {decl} }}")).expect("parses");
            let input = InterfaceInput::from_bind(input).expect("no fields");
            let err = expand_interface(&input).expect_err("reserved method must be rejected");
            assert!(err.to_string().contains("library-reserved"), "{decl}: {err}");
        }
        // A domain overload with a different signature is fine.
        let input = parse(quote! {
            "C" => C { fn toString(n: i32) -> String; }
        });
        assert!(expand_interface(&input).is_ok(), "toString(int) is not reserved");
        let input = parse(quote! {
            "C" => C { fn equals(o: String) -> bool; }
        });
        assert!(expand_interface(&input).is_ok(), "equals(String) is not reserved");
    }

    #[test]
    fn unsupported_types_are_rejected() {
        for decl in [
            "fn f(a: Self) -> i32;",
            "fn f() -> Self;",
            "fn f(a: &str) -> i32;",
            "fn f() -> &str;",
        ] {
            let input: super::BindInput =
                syn::parse_str(&format!("\"C\" => C {{ {decl} }}")).expect("parses");
            let input = InterfaceInput::from_bind(input).expect("no fields");
            let err = expand_interface(&input).expect_err("invalid type must be rejected");
            assert!(err.to_string().contains("interface!"), "{decl}: {err}");
        }
    }

    #[test]
    fn reserved_param_name_is_rejected() {
        let input: super::BindInput = syn::parse2(quote! {
            "C" => C { fn f(env: String) -> i32; }
        })
        .expect("parses");
        let input = InterfaceInput::from_bind(input).expect("no fields");
        let err = expand_interface(&input).expect_err("a param named env must be rejected");
        assert!(err.to_string().contains("`env` is reserved"), "{err}");
    }
}
