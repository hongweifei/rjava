//! The invocation machinery: JNI method/field resolution, argument
//! conversion, and — critically — pending-Java-exception handling.
//!
//! Every operation here follows the same pattern:
//!
//! 1. build the JNI signature string from the **types** involved
//!    (`ToJava::java_args` for the argument list, `FromJava::java_return_type`
//!    for the return type) and parse it into a [`MethodSignature`] /
//!    [`FieldSignature`] (the `jni` crate validates argument counts and
//!    primitive types against it before calling),
//! 2. convert Rust arguments into owned [`JavaArg`]s, then borrow them as
//!    [`jni::JValue`]s for the duration of the call,
//! 3. perform the JNI call wrapped in [`with_check`], which translates any
//!    pending Java exception into [`JavaError::JavaException`] **and clears
//!    it**, so a failed call never poisons the current thread,
//! 4. convert the result with the caller's [`FromJava`] annotation.
//!
//! ## Method signatures and the return type
//!
//! Modern JVMs (HotSpot / OpenJDK 17+, and others) match `GetMethodID`
//! against the **full** method signature *including the return type* — this
//! was verified empirically against JDK 21. Older HotSpot ignored the return
//! type, but we cannot rely on that. Consequently, the signature built from
//! the caller's annotation must match the real method exactly: if the
//! annotation's return fragment is generic (`Ljava/lang/Object;` for
//! [`JObject`], or `V` for `()`/`call_void`), the initial lookup fails and
//! the [reflection fallback](#method-signature-resolution-with-a-reflection-fallback)
//! resolves the exact signature and retries. The fallback matches each
//! candidate's parameters in three passes — **exact** first, then
//! **boxing** (a primitive argument for an `Object`-typed or wrapper-typed
//! parameter becomes `Wrapper.valueOf(...)`, which is what makes
//! `list.call_void("add", (10_i32,))` work on `ArrayList.add(Object)`), then
//! **unboxing** (a wrapper object argument for a primitive parameter is read
//! back via `Wrapper.xxxValue()`, e.g. an `Integer` object for an `int`
//! parameter).
//! Field lookup likewise compares the full type signature, so
//! `get_field`/`set_field` require the *exact* Java field type (see
//! [`JObject::get_field`](crate::JObject::get_field)).

use std::sync::Arc;

use jni::objects::{Global, JClass, JObject, JString};
use jni::signature::{MethodSignature, RuntimeFieldSignature, RuntimeMethodSignature};
use jni::strings::JNIString;
use jni::{Env, JValue, JValueOwned};

use crate::array::ArrayKind;
use crate::convert::{to_jvalue, FromJava, JavaArg, ToJava};
use crate::error::{JavaError, JavaResult};
use crate::handles::JObject as JObjectHandle;

// ---------------------------------------------------------------------------
// Exception handling
// ---------------------------------------------------------------------------

/// Translate a pending Java exception into a [`JavaError::JavaException`],
/// reading the class name and message **before** clearing it.
fn capture_exception(env: &mut Env<'_>) -> JavaResult<()> {
    match env.exception_catch() {
        Ok(()) => Err(JavaError::JavaException {
            class: String::new(),
            message: String::new(),
        }),
        Err(jni::errors::Error::CaughtJavaException { name, msg, .. }) => {
            Err(JavaError::JavaException {
                class: name,
                message: msg,
            })
        }
        Err(e) => Err(JavaError::from(e)),
    }
}

/// If a Java exception is pending, capture + clear it and return it as
/// [`JavaError::JavaException`]; otherwise return `Ok(())`.
pub(crate) fn check_exception(env: &mut Env<'_>) -> JavaResult<()> {
    if env.exception_check() {
        capture_exception(env)
    } else {
        Ok(())
    }
}

/// Run a JNI operation, then capture and clear any pending Java exception it
/// left behind.
///
/// If the operation failed *and* a pending exception exists, the exception
/// wins: it is the ground truth for what went wrong, and its class + message
/// are far more useful than the raw JNI error.
pub(crate) fn with_check<'env, T>(
    env: &mut Env<'env>,
    f: impl FnOnce(&mut Env<'env>) -> jni::errors::Result<T>,
) -> JavaResult<T> {
    let r = f(env).map_err(JavaError::from);
    match r {
        Ok(v) => {
            check_exception(env)?;
            Ok(v)
        }
        Err(e) => {
            if env.exception_check() {
                // A pending Java exception is the ground truth: capture it
                // (clearing it) and report the exception instead.
                match capture_exception(env) {
                    Err(exc) => Err(exc),
                    Ok(()) => Err(e),
                }
            } else {
                Err(e)
            }
        }
    }
}

/// Same as [`with_check`], but for an operation that already returns a
/// `JavaResult` (e.g. a `FromJava` conversion that may itself trigger a JNI
/// error).
pub(crate) fn finish<'env, T>(env: &mut Env<'env>, r: JavaResult<T>) -> JavaResult<T> {
    if env.exception_check() {
        match capture_exception(env) {
            Err(exc) => Err(exc),
            Ok(()) => r,
        }
    } else {
        r
    }
}

// ---------------------------------------------------------------------------
// Classes
// ---------------------------------------------------------------------------

/// Look up a class by its slash-separated JNI name (`java/lang/String`).
pub(crate) fn find_class<'env>(
    env: &mut Env<'env>,
    name: JNIString,
) -> JavaResult<JClass<'env>> {
    with_check(env, |env| env.find_class(name))
}

/// The runtime class of an object.
pub(crate) fn get_object_class<'env>(
    env: &mut Env<'env>,
    obj: &JObject<'env>,
) -> JavaResult<JClass<'env>> {
    with_check(env, |env| env.get_object_class(obj))
}

/// The binary name (`Class.getName()`) of a class reference.
pub(crate) fn class_name(env: &mut Env<'_>, cls: &JClass<'_>) -> JavaResult<String> {
    let name: JString = with_check(env, |env| cls.get_name(env))?;
    finish(env, Ok(name.mutf8_chars(env)?.into()))
}

// ---------------------------------------------------------------------------
// Signatures
// ---------------------------------------------------------------------------

fn parse_method_sig(args: &str, ret: &str) -> JavaResult<RuntimeMethodSignature> {
    let full = format!("({args}){ret}");
    RuntimeMethodSignature::from_str(&full).map_err(JavaError::from)
}

fn parse_field_sig(frag: &str) -> JavaResult<RuntimeFieldSignature> {
    RuntimeFieldSignature::from_str(frag).map_err(JavaError::from)
}

// ---------------------------------------------------------------------------
// Method signature resolution with a reflection fallback
// ---------------------------------------------------------------------------

/// Convert a `java.lang.Class` name (as returned by `Class.getName()`) into a
/// JNI type fragment: `int` → `I`, `java.lang.String` →
/// `Ljava/lang/String;`, `[Ljava.lang.String;` → `[Ljava/lang/String;`.
fn class_name_to_fragment(name: &str) -> String {
    match name {
        "boolean" => "Z".to_string(),
        "byte" => "B".to_string(),
        "char" => "C".to_string(),
        "short" => "S".to_string(),
        "int" => "I".to_string(),
        "long" => "J".to_string(),
        "float" => "F".to_string(),
        "double" => "D".to_string(),
        "void" => "V".to_string(),
        _ if name.starts_with('[') => name.replace('.', "/"),
        _ => format!("L{};", name.replace('.', "/")),
    }
}

/// Read `Class.getName()` on a `Class` object as a Rust `String`.
///
/// `pub(crate)`: also used by `rjava::interface` (feature `interface`) to
/// read the primitive parameter names of a proxied method for unboxing.
pub(crate) fn class_name_of<'env>(env: &mut Env<'env>, class: &JObject<'env>) -> JavaResult<String> {
    let name: JValueOwned = with_check(env, |env| {
        env.call_method(class, jni::jni_str!("getName"), jni::jni_sig!("()Ljava/lang/String;"), &[])
    })?;
    let jstr = match name {
        JValueOwned::Object(o) => env.cast_local::<JString>(o)?,
        _ => {
            return Err(JavaError::InvalidArgument(
                "Class.getName() did not return a String",
            ))
        }
    };
    finish(env, Ok(jstr.mutf8_chars(env)?.into()))
}

/// Check whether a `Class` object represents the primitive type with the
/// given JNI letter (e.g. `I` for `int`).
fn is_primitive_class<'env>(
    env: &mut Env<'env>,
    class: &JObject<'env>,
    letter: &str,
) -> JavaResult<bool> {
    let name = class_name_of(env, class)?;
    Ok(letter == "Z" && name == "boolean"
        || letter == "B" && name == "byte"
        || letter == "C" && name == "char"
        || letter == "S" && name == "short"
        || letter == "I" && name == "int"
        || letter == "J" && name == "long"
        || letter == "F" && name == "float"
        || letter == "D" && name == "double")
}

/// Is the binary class name (as returned by `Class.getName()`) one of the
/// eight JVM primitive names (`int`, `long`, …)?
///
/// `pub(crate)`: also used by `rjava::interface` (feature `interface`) to
/// decide which proxied method arguments need unboxing.
pub(crate) fn is_primitive_name(name: &str) -> bool {
    matches!(
        name,
        "boolean" | "byte" | "char" | "short" | "int" | "long" | "float" | "double"
    )
}

/// The runtime class of an object (`Object.getClass()`).
fn runtime_class_of<'env>(
    env: &mut Env<'env>,
    obj: &JObject<'env>,
) -> JavaResult<JObject<'env>> {
    let c: JValueOwned = with_check(env, |env| {
        env.call_method(obj, jni::jni_str!("getClass"), jni::jni_sig!("()Ljava/lang/Class;"), &[])
    })?;
    match c {
        JValueOwned::Object(o) => Ok(o),
        _ => Err(JavaError::InvalidArgument(
            "Object.getClass() did not return a Class",
        )),
    }
}

/// Does `param_class.isAssignableFrom(arg_class)` hold?
fn is_assignable<'env>(
    env: &mut Env<'env>,
    param_class: &JObject<'env>,
    arg_class: &JObject<'env>,
) -> JavaResult<bool> {
    let r: JValueOwned = with_check(env, |env| {
        env.call_method(
            param_class,
            jni::jni_str!("isAssignableFrom"),
            jni::jni_sig!("(Ljava/lang/Class;)Z"),
            &[JValue::Object(arg_class)],
        )
    })?;
    match r {
        JValueOwned::Bool(b) => Ok(b),
        _ => Err(JavaError::InvalidArgument(
            "Class.isAssignableFrom() did not return a boolean",
        )),
    }
}

/// Does `arg` (one of my converted arguments) match a method parameter of
/// class `param_class`?
///
/// Primitives must match the exact primitive class; object arguments must be
/// *assignable* to the parameter type (checked via `isAssignableFrom`), which
/// also handles `JObject`-typed arguments whose runtime type is a subtype.
/// Null object arguments match any **reference** parameter — never a
/// primitive one (a null can be unboxed by no wrapper).
fn arg_matches_param<'env>(
    env: &mut Env<'env>,
    arg: &JavaArg<'env>,
    param_class: &JObject<'env>,
) -> JavaResult<bool> {
    match arg {
        JavaArg::Bool(_) => is_primitive_class(env, param_class, "Z"),
        JavaArg::Byte(_) => is_primitive_class(env, param_class, "B"),
        JavaArg::Char(_) => is_primitive_class(env, param_class, "C"),
        JavaArg::Short(_) => is_primitive_class(env, param_class, "S"),
        JavaArg::Int(_) => is_primitive_class(env, param_class, "I"),
        JavaArg::Long(_) => is_primitive_class(env, param_class, "J"),
        JavaArg::Float(_) => is_primitive_class(env, param_class, "F"),
        JavaArg::Double(_) => is_primitive_class(env, param_class, "D"),
        JavaArg::Object(o) if o.is_null() => {
            Ok(!is_primitive_name(&class_name_of(env, param_class)?))
        }
        JavaArg::Object(o) => {
            let arg_class = runtime_class_of(env, o)?;
            is_assignable(env, param_class, &arg_class)
        }
    }
}

/// Is `arg` a primitive (as opposed to an object reference)?
fn is_primitive_arg(arg: &JavaArg<'_>) -> bool {
    !matches!(arg, JavaArg::Object(_))
}

/// Can a primitive argument be *boxed* into `class`? That is, is `class` one
/// of the eight wrapper classes (`java.lang.Integer` etc.), or
/// `java.lang.Object` itself — which is how generics erase, so
/// `ArrayList.add(Object)` / `HashMap.put(Object, Object)` are the methods
/// this pass makes primitive calls resolve against.
fn is_boxable_param_class<'env>(
    env: &mut Env<'env>,
    class: &JObject<'env>,
) -> JavaResult<bool> {
    let name = class_name_of(env, class)?;
    Ok(name == "java.lang.Object"
        || matches!(
            name.as_str(),
            "java.lang.Boolean"
                | "java.lang.Byte"
                | "java.lang.Character"
                | "java.lang.Short"
                | "java.lang.Integer"
                | "java.lang.Long"
                | "java.lang.Float"
                | "java.lang.Double"
        ))
}

/// Box a primitive argument into its wrapper class via `Wrapper.valueOf` —
/// `10_i32` → `Integer.valueOf(10)`. Used by the reflection fallback when a
/// method's parameter is `Object`-typed (or one of the wrapper classes), and
/// by `rjava::interface` (feature `interface`) to box a handler's primitive
/// return values for the proxy machinery.
pub(crate) fn box_primitive<'env>(env: &mut Env<'env>, arg: &JavaArg<'env>) -> JavaResult<JObject<'env>> {
    let (cls_name, sig, value) = match arg {
        JavaArg::Bool(v) => ("java/lang/Boolean", "(Z)Ljava/lang/Boolean;", JValue::Bool(*v)),
        JavaArg::Byte(v) => ("java/lang/Byte", "(B)Ljava/lang/Byte;", JValue::Byte(*v)),
        JavaArg::Char(v) => ("java/lang/Character", "(C)Ljava/lang/Character;", JValue::Char(*v)),
        JavaArg::Short(v) => ("java/lang/Short", "(S)Ljava/lang/Short;", JValue::Short(*v)),
        JavaArg::Int(v) => ("java/lang/Integer", "(I)Ljava/lang/Integer;", JValue::Int(*v)),
        JavaArg::Long(v) => ("java/lang/Long", "(J)Ljava/lang/Long;", JValue::Long(*v)),
        JavaArg::Float(v) => ("java/lang/Float", "(F)Ljava/lang/Float;", JValue::Float(*v)),
        JavaArg::Double(v) => ("java/lang/Double", "(D)Ljava/lang/Double;", JValue::Double(*v)),
        JavaArg::Object(_) => {
            return Err(JavaError::InvalidArgument(
                "internal error: cannot box an object argument",
            ))
        }
    };
    let cls = find_class(env, JNIString::from(cls_name))?;
    let rms = RuntimeMethodSignature::from_str(sig).map_err(JavaError::from)?;
    let msig: MethodSignature = (&rms).into();
    let out: JValueOwned = with_check(env, |env| {
        env.call_static_method(&cls, jni::jni_str!("valueOf"), msig, &[value])
    })?;
    match out {
        JValueOwned::Object(o) => Ok(o),
        _ => Err(JavaError::InvalidArgument(
            "internal error: wrapper valueOf did not return an object",
        )),
    }
}

/// Unbox a wrapper object argument into its primitive value via
/// `Wrapper.xxxValue()` — an `Integer` → `intValue()`. Used by the
/// reflection fallback when a method's parameter is a **primitive** class and
/// the argument is a wrapper object.
///
/// Java's unboxing requires the *exact* wrapper type: an `Integer` unboxes to
/// `int` only, a `Number` does not unbox, and there are **no widening
/// conversions** (an `Integer` does not match a `long` parameter). Null
/// object arguments never unbox — they simply do not match a primitive
/// parameter, so the resolver reports "no candidate" and the caller sees the
/// standard could-not-resolve error instead of an NPE from here.
fn unbox_primitive<'env>(
    env: &mut Env<'env>,
    arg: &JavaArg<'env>,
    param_name: &str,
) -> JavaResult<Option<JavaArg<'env>>> {
    // Only *object* arguments can be unboxed, and only non-null ones.
    let JavaArg::Object(o) = arg else {
        return Ok(None);
    };
    if o.is_null() {
        return Ok(None);
    }
    // The argument's runtime class must be exactly the wrapper for the
    // primitive parameter class.
    let runtime = runtime_class_of(env, o)?;
    let runtime_name = class_name_of(env, &runtime)?;
    let value: JValueOwned = match (param_name, runtime_name.as_str()) {
        ("boolean", "java.lang.Boolean") => with_check(env, |env| {
            env.call_method(o, jni::jni_str!("booleanValue"), jni::jni_sig!("()Z"), &[])
        })?,
        ("byte", "java.lang.Byte") => with_check(env, |env| {
            env.call_method(o, jni::jni_str!("byteValue"), jni::jni_sig!("()B"), &[])
        })?,
        ("char", "java.lang.Character") => with_check(env, |env| {
            env.call_method(o, jni::jni_str!("charValue"), jni::jni_sig!("()C"), &[])
        })?,
        ("short", "java.lang.Short") => with_check(env, |env| {
            env.call_method(o, jni::jni_str!("shortValue"), jni::jni_sig!("()S"), &[])
        })?,
        ("int", "java.lang.Integer") => with_check(env, |env| {
            env.call_method(o, jni::jni_str!("intValue"), jni::jni_sig!("()I"), &[])
        })?,
        ("long", "java.lang.Long") => with_check(env, |env| {
            env.call_method(o, jni::jni_str!("longValue"), jni::jni_sig!("()J"), &[])
        })?,
        ("float", "java.lang.Float") => with_check(env, |env| {
            env.call_method(o, jni::jni_str!("floatValue"), jni::jni_sig!("()F"), &[])
        })?,
        ("double", "java.lang.Double") => with_check(env, |env| {
            env.call_method(o, jni::jni_str!("doubleValue"), jni::jni_sig!("()D"), &[])
        })?,
        _ => return Ok(None),
    };
    Ok(Some(match value {
        JValueOwned::Bool(b) => JavaArg::Bool(b),
        JValueOwned::Byte(b) => JavaArg::Byte(b),
        JValueOwned::Char(c) => JavaArg::Char(c),
        JValueOwned::Short(s) => JavaArg::Short(s),
        JValueOwned::Int(i) => JavaArg::Int(i),
        JValueOwned::Long(l) => JavaArg::Long(l),
        JValueOwned::Float(f) => JavaArg::Float(f),
        JValueOwned::Double(d) => JavaArg::Double(d),
        _ => {
            return Err(JavaError::InvalidArgument(
                "internal error: wrapper xxxValue() did not return a primitive",
            ))
        }
    }))
}

/// One matched parameter list: the JNI fragments of the parameter classes
/// and, per parameter, the replacement argument (`None` when the original
/// argument stands — an exact match). A `Some` replacement is either the
/// boxed wrapper object (boxing pass) or the unboxed primitive (unboxing
/// pass).
type MatchedParams<'env> = (Vec<String>, Vec<Option<JavaArg<'env>>>);

/// Match `args` against the parameter classes of `params_arr` (a `Class[]`
/// from `Method.getParameterTypes()` or `Constructor.getParameterTypes()`).
///
/// Each argument position is tried in three passes:
///
/// 1. **Exact** — [`arg_matches_param`]: a primitive argument matches the
///    primitive parameter, an object argument must be assignable to the
///    reference parameter, a null object matches any reference parameter.
/// 2. **Box** — a primitive argument with an `Object`-typed or wrapper-typed
///    parameter is boxed via [`box_primitive`] (`Wrapper.valueOf`).
/// 3. **Unbox** — a non-null wrapper object argument with a primitive
///    parameter is unboxed via [`unbox_primitive`] (`Wrapper.xxxValue()`),
///    requiring the exact wrapper type; null objects never match primitives
///    and no widening conversions apply.
///
/// Returns the JNI fragments of the matched parameter classes and, per
/// parameter, the replacement argument (`None` when the original argument
/// stands). `Ok(None)` when any argument fails to match.
fn match_param_classes<'env>(
    env: &mut Env<'env>,
    params_arr: &jni::objects::JObjectArray<'env>,
    args: &[JavaArg<'env>],
) -> JavaResult<Option<MatchedParams<'env>>> {
    let pn = params_arr.len(env)?;
    if pn != args.len() {
        return Ok(None);
    }
    let mut fragments = Vec::with_capacity(pn);
    let mut replacement = Vec::with_capacity(pn);
    for (i, arg) in args.iter().enumerate() {
        let c: JObject = params_arr.get_element(env, i)?;
        // Pass 1: exact match.
        if arg_matches_param(env, arg, &c)? {
            fragments.push(class_name_to_fragment(&class_name_of(env, &c)?));
            replacement.push(None);
            continue;
        }
        // Pass 2: boxing (primitive arg + Object/wrapper param).
        if is_primitive_arg(arg) && is_boxable_param_class(env, &c)? {
            let b = box_primitive(env, arg)?;
            fragments.push(class_name_to_fragment(&class_name_of(env, &c)?));
            replacement.push(Some(JavaArg::Object(b)));
            continue;
        }
        // Pass 3: unboxing (wrapper object arg + primitive param).
        let name = class_name_of(env, &c)?;
        if let Some(unboxed) = unbox_primitive(env, arg, &name)? {
            fragments.push(class_name_to_fragment(&name));
            replacement.push(Some(unboxed));
            continue;
        }
        return Ok(None);
    }
    Ok(Some((fragments, replacement)))
}

/// Resolve the exact JNI signature of a method by name, matching the
/// parameter types of `args`, and return the (possibly adjusted) argument
/// list to retry the call with.
///
/// This is the fallback used when the signature derived from the caller's
/// annotation does not match exactly: HotSpot (and other JVMs) match
/// `GetMethodID` against the *full* signature including the return type, so a
/// generic annotation like `JObject` or `()` needs the real signature. We
/// enumerate `Class.getMethods()`, keep the candidates whose parameter types
/// accept our arguments (see [`match_param_classes`] for the three passes —
/// exact, then boxing primitives into `Integer`/`Double`/… for `Object`-typed
/// parameters, which is what makes `list.call_void("add", (10_i32,))` work,
/// then unboxing wrapper objects for primitive parameters, e.g. an `Integer`
/// for an `int`), and let `GetMethodID` disambiguate the rest. Candidates are
/// tried in `Class.getMethods()` order and the first match wins
/// (deterministic).
///
/// Exact matches return the original `args` unchanged; boxed/unboxed matches
/// return an adjusted list in which the affected arguments have been replaced
/// by the wrapper objects / primitive values.
///
/// `pub(crate)` because the `bind!` machinery (`crate::bind`) reuses this
/// fallback for its declared `JObject` annotations and wrong-type calls.
pub(crate) fn resolve_exact_method_sig<'env>(
    env: &mut Env<'env>,
    class: &JClass<'env>,
    name: &str,
    args: Vec<JavaArg<'env>>,
    is_static: bool,
) -> JavaResult<(RuntimeMethodSignature, Vec<JavaArg<'env>>)> {
    let name_j = JNIString::from(name);

    // class.getMethods() -> Method[]
    let methods: JValueOwned = with_check(env, |env| {
        env.call_method(class, jni::jni_str!("getMethods"), jni::jni_sig!("()[Ljava/lang/reflect/Method;"), &[])
    })?;
    let methods_arr: jni::objects::JObjectArray<'env> = match methods {
        JValueOwned::Object(o) => jni::objects::JObjectArray::<JObject>::cast_local(env, o)?,
        _ => {
            return Err(JavaError::InvalidArgument(
                "Class.getMethods() did not return an array",
            ))
        }
    };

    let n = methods_arr.len(env)?;
    for i in 0..n {
        let m: JObject = methods_arr.get_element(env, i)?;
        // Method.getName()
        let mname: JValueOwned = with_check(env, |env| {
            env.call_method(&m, jni::jni_str!("getName"), jni::jni_sig!("()Ljava/lang/String;"), &[])
        })?;
        let mname_jstr = match mname {
            JValueOwned::Object(o) => env.cast_local::<JString>(o)?,
            _ => continue,
        };
        let mname: String = mname_jstr.mutf8_chars(env)?.into();
        if mname != name {
            continue;
        }

        // Method.getParameterTypes() -> Class[]
        let params: JValueOwned = with_check(env, |env| {
            env.call_method(&m, jni::jni_str!("getParameterTypes"), jni::jni_sig!("()[Ljava/lang/Class;"), &[])
        })?;
        let params_arr: jni::objects::JObjectArray<'env> = match params {
            JValueOwned::Object(o) => jni::objects::JObjectArray::<JObject>::cast_local(env, o)?,
            _ => {
                return Err(JavaError::InvalidArgument(
                    "Method.getParameterTypes() did not return an array",
                ))
            }
        };
        let Some((fragments, replacement)) = match_param_classes(env, &params_arr, &args)? else {
            continue;
        };

        // Method.getReturnType() -> Class
        let ret: JValueOwned = with_check(env, |env| {
            env.call_method(&m, jni::jni_str!("getReturnType"), jni::jni_sig!("()Ljava/lang/Class;"), &[])
        })?;
        let ret_class = match ret {
            JValueOwned::Object(o) => o,
            _ => {
                return Err(JavaError::InvalidArgument(
                    "Method.getReturnType() did not return a Class",
                ))
            }
        };
        let ret_frag = class_name_to_fragment(&class_name_of(env, &ret_class)?);

        let full = format!("({}){}", fragments.join(""), ret_frag);
        let rms = match RuntimeMethodSignature::from_str(&full) {
            Ok(sig) => sig,
            Err(_) => continue,
        };
        let sig: MethodSignature = (&rms).into();
        // GetMethodID resolves *instance* methods, GetStaticMethodID resolves
        // *static* ones — the same method name may exist in both namespaces.
        let resolved: jni::errors::Result<()> = if is_static {
            env.get_static_method_id(class, name_j.clone(), sig).map(|_| ())
        } else {
            env.get_method_id(class, name_j.clone(), sig).map(|_| ())
        };
        if resolved.is_ok() {
            let adjusted: Vec<JavaArg<'env>> = args
                .into_iter()
                .zip(replacement)
                .map(|(a, r)| r.unwrap_or(a))
                .collect();
            return Ok((rms, adjusted));
        }
        check_exception(env)?;
    }

    Err(JavaError::InvalidArgument(
        "could not resolve the method signature via reflection;          check that the argument types match the method's parameters",
    ))
}

// ---------------------------------------------------------------------------
// Registration-time return-type resolution (the native-method fallback)
// ---------------------------------------------------------------------------

/// Split a validated JNI method signature into its parameter fragments and
/// return fragment: `(Ljava/lang/String;[I)Z` →
/// (`["Ljava/lang/String;", "[I"]`, `"Z"`).
///
/// The `jni` crate's [`RuntimeMethodSignature`] deliberately discards object
/// class names (its [`jni::signature::JavaType`] only distinguishes
/// primitives from references), so the raw string is split here instead —
/// the signature has already been validated with the parser, so this cannot
/// fail on well-formed input.
fn split_method_sig(sig: &str) -> Option<(Vec<String>, String)> {
    let open = sig.find('(')?;
    let close = sig.rfind(')')?;
    let params = &sig[open + 1..close];
    let ret = &sig[close + 1..];

    let mut fragments = Vec::new();
    let mut rest = params;
    while !rest.is_empty() {
        // One JNI type fragment: a primitive letter, `L…;` (object), or
        // `[…]` (array) followed by a primitive letter or `L…;`.
        let len = match rest.chars().next()? {
            'Z' | 'B' | 'C' | 'S' | 'I' | 'J' | 'F' | 'D' => 1,
            'L' => rest.find(';')? + 1,
            '[' => {
                let mut n = 1;
                let mut base = &rest[n..];
                while let Some(b) = base.strip_prefix('[') {
                    n += 1;
                    base = b;
                }
                match base.chars().next()? {
                    'L' => n + base.find(';')? + 1,
                    c if "ZBCSIJFD".contains(c) => n + 1,
                    _ => return None,
                }
            }
            _ => return None,
        };
        fragments.push(rest[..len].to_string());
        rest = &rest[len..];
    }
    Some((fragments, ret.to_string()))
}

/// Read `Method.getName()` from a `java.lang.reflect.Method` as a Rust
/// `String`.
fn method_name<'env>(env: &mut Env<'env>, m: &JObject<'env>) -> JavaResult<String> {
    let name: JValueOwned = with_check(env, |env| {
        env.call_method(m, jni::jni_str!("getName"), jni::jni_sig!("()Ljava/lang/String;"), &[])
    })?;
    let jstr = match name {
        JValueOwned::Object(o) => env.cast_local::<JString>(o)?,
        _ => {
            return Err(JavaError::InvalidArgument(
                "Method.getName() did not return a String",
            ))
        }
    };
    finish(env, Ok(jstr.mutf8_chars(env)?.into()))
}

/// Find the declared method `name` whose parameter fragments equal `params`,
/// and return the JNI fragment of its return type — or `None` if no such
/// method is declared on `class`. Name plus parameter types uniquely identify
/// a Java method (overloading on the return type alone is illegal), so the
/// first match in `getDeclaredMethods()` order is deterministic.
fn declared_method_return_fragment<'env>(
    env: &mut Env<'env>,
    class: &JClass<'env>,
    name: &str,
    params: &[String],
) -> JavaResult<Option<String>> {
    // class.getDeclaredMethods() -> Method[]
    let methods: JValueOwned = with_check(env, |env| {
        env.call_method(class, jni::jni_str!("getDeclaredMethods"), jni::jni_sig!("()[Ljava/lang/reflect/Method;"), &[])
    })?;
    let methods_arr: jni::objects::JObjectArray<'env> = match methods {
        JValueOwned::Object(o) => jni::objects::JObjectArray::<JObject>::cast_local(env, o)?,
        _ => {
            return Err(JavaError::InvalidArgument(
                "Class.getDeclaredMethods() did not return an array",
            ))
        }
    };

    let n = methods_arr.len(env)?;
    for i in 0..n {
        let m: JObject = methods_arr.get_element(env, i)?;
        if method_name(env, &m)? != name {
            continue;
        }

        // Method.getParameterTypes() -> Class[]
        let ptypes: JValueOwned = with_check(env, |env| {
            env.call_method(&m, jni::jni_str!("getParameterTypes"), jni::jni_sig!("()[Ljava/lang/Class;"), &[])
        })?;
        let ptypes_arr: jni::objects::JObjectArray<'env> = match ptypes {
            JValueOwned::Object(o) => jni::objects::JObjectArray::<JObject>::cast_local(env, o)?,
            _ => {
                return Err(JavaError::InvalidArgument(
                    "Method.getParameterTypes() did not return an array",
                ))
            }
        };

        let pn = ptypes_arr.len(env)?;
        if pn != params.len() {
            continue;
        }
        let mut frags = Vec::with_capacity(pn);
        for j in 0..pn {
            let pc: JObject = ptypes_arr.get_element(env, j)?;
            frags.push(class_name_to_fragment(&class_name_of(env, &pc)?));
        }
        if frags != params {
            continue;
        }

        // Method.getReturnType() -> Class
        let ret: JValueOwned = with_check(env, |env| {
            env.call_method(&m, jni::jni_str!("getReturnType"), jni::jni_sig!("()Ljava/lang/Class;"), &[])
        })?;
        let ret_class = match ret {
            JValueOwned::Object(o) => o,
            _ => {
                return Err(JavaError::InvalidArgument(
                    "Method.getReturnType() did not return a Class",
                ))
            }
        };
        return Ok(Some(class_name_to_fragment(&class_name_of(env, &ret_class)?)));
    }
    Ok(None)
}

/// Registration-time fallback for [`crate::JClass::register_natives`].
///
/// When a batch registration fails with `NoSuchMethodError`, the type-derived
/// descriptors whose derived **return** fragment is the deliberately-generic
/// `Ljava/lang/Object;` / `[Ljava/lang/Object;` marker (the `JObject` /
/// `Vec<JObject>` / `Option<JObject>` annotations) may simply mismatch the
/// concrete Java return type: modern JVMs match `RegisterNatives` against the
/// **full** descriptor, return type included. This resolves the exact return
/// type of each such method via reflection (`Class.getDeclaredMethods()`,
/// matching on name + parameter types — the parameters of a derived
/// descriptor are exact, only the return is generic) and returns corrected
/// signatures.
///
/// Returns `Ok(Some(corrected))` — a `Vec` parallel to `methods` holding the
/// corrected signature (or the original one for non-qualifying entries) — if
/// at least one type-derived method qualified **and** every qualifying method
/// resolved. Returns `Ok(None)` when nothing qualified or a qualifying method
/// could not be resolved (the caller then surfaces the original
/// `NoSuchMethodError` unchanged, keeping genuine user bugs visible).
///
/// The explicit-signature form is never touched (`NativeMethod::call` is
/// `None` for it): the user wrote that signature, so its errors are theirs.
/// Note the corrected descriptor changes only the JVM's method-ID lookup —
/// the trampoline's C ABI is unchanged, since the type-derived trampolines
/// already return `JObject` for every reference type regardless of the
/// concrete class.
pub(crate) fn resolve_derived_native_sigs<'env>(
    env: &mut Env<'env>,
    class: &JClass<'env>,
    methods: &[crate::native::NativeMethod],
) -> JavaResult<Option<Vec<String>>> {
    // Which entries are type-derived with a generic return marker?
    let mut qualified: Vec<usize> = Vec::new();
    let mut parsed: Vec<Option<(Vec<String>, String)>> = Vec::with_capacity(methods.len());
    for (i, m) in methods.iter().enumerate() {
        if m.call.is_none() {
            parsed.push(None); // explicit-signature: never auto-correct
            continue;
        }
        // Validate with the jni parser (as `NativeMethod::new` does), then
        // split the raw string — only the return marker decides eligibility.
        let ok = RuntimeMethodSignature::from_str(m.sig()).is_ok();
        match ok.then(|| split_method_sig(m.sig())).flatten() {
            Some((params, ret))
                if ret == "Ljava/lang/Object;" || ret == "[Ljava/lang/Object;" =>
            {
                qualified.push(i);
                parsed.push(Some((params, ret)));
            }
            _ => parsed.push(None), // derived but exact return: not our failure
        }
    }
    if qualified.is_empty() {
        return Ok(None);
    }

    let mut corrected: Vec<String> = methods.iter().map(|m| m.sig().to_string()).collect();
    for &i in &qualified {
        let (params, _) = parsed[i].as_ref().expect("qualified entries parse");
        match declared_method_return_fragment(env, class, methods[i].name(), params)? {
            Some(ret_frag) => {
                corrected[i] = format!("({}){}", params.join(""), ret_frag);
            }
            // The method genuinely does not exist (or the params do not
            // match): the original NoSuchMethodError stays visible.
            None => return Ok(None),
        }
    }
    Ok(Some(corrected))
}

/// Resolve the exact signature of a constructor (JNI name `<init>`) by
/// enumerating `Class.getConstructors()` — the constructor analog of
/// [`resolve_exact_method_sig`], with the same three matching passes
/// (exact/box/unbox) and a `V` return type. Candidates are tried in
/// `Class.getConstructors()` order and the first match wins (deterministic).
fn resolve_exact_ctor_sig<'env>(
    env: &mut Env<'env>,
    class: &JClass<'env>,
    args: Vec<JavaArg<'env>>,
) -> JavaResult<(RuntimeMethodSignature, Vec<JavaArg<'env>>)> {
    // class.getConstructors() -> Constructor[]
    let ctors: JValueOwned = with_check(env, |env| {
        env.call_method(class, jni::jni_str!("getConstructors"), jni::jni_sig!("()[Ljava/lang/reflect/Constructor;"), &[])
    })?;
    let ctors_arr: jni::objects::JObjectArray<'env> = match ctors {
        JValueOwned::Object(o) => jni::objects::JObjectArray::<JObject>::cast_local(env, o)?,
        _ => {
            return Err(JavaError::InvalidArgument(
                "Class.getConstructors() did not return an array",
            ))
        }
    };

    let n = ctors_arr.len(env)?;
    for i in 0..n {
        let ctor: JObject = ctors_arr.get_element(env, i)?;
        // Constructor.getParameterTypes() -> Class[]
        let params: JValueOwned = with_check(env, |env| {
            env.call_method(&ctor, jni::jni_str!("getParameterTypes"), jni::jni_sig!("()[Ljava/lang/Class;"), &[])
        })?;
        let params_arr: jni::objects::JObjectArray<'env> = match params {
            JValueOwned::Object(o) => jni::objects::JObjectArray::<JObject>::cast_local(env, o)?,
            _ => {
                return Err(JavaError::InvalidArgument(
                    "Constructor.getParameterTypes() did not return an array",
                ))
            }
        };
        let Some((fragments, replacement)) = match_param_classes(env, &params_arr, &args)? else {
            continue;
        };

        let full = format!("({})V", fragments.join(""));
        let rms = match RuntimeMethodSignature::from_str(&full) {
            Ok(sig) => sig,
            Err(_) => continue,
        };
        let sig: MethodSignature = (&rms).into();
        if env.get_method_id(class, jni::jni_str!("<init>"), sig).is_ok() {
            let adjusted: Vec<JavaArg<'env>> = args
                .into_iter()
                .zip(replacement)
                .map(|(a, r)| r.unwrap_or(a))
                .collect();
            return Ok((rms, adjusted));
        }
        check_exception(env)?;
    }

    Err(JavaError::InvalidArgument(
        "could not resolve a constructor signature via reflection; check that the argument types match one of the class's constructors",
    ))
}

// ---------------------------------------------------------------------------
// Operation context (`JavaError::WithContext`)
// ---------------------------------------------------------------------------

/// The Java-source-style name of a JNI type fragment: `I` → `int`,
/// `Ljava/lang/String;` → `String` (the **simple** name), `[I` → `int[]`,
/// `[[Ljava/lang/Integer;` → `Integer[][]`. Primitives render as the Java
/// keywords; reference types render as their simple name so the operation
/// string reads like Java source — the target class, printed separately with
/// its dotted binary name, disambiguates the common case.
fn fragment_to_java_name(fragment: &str) -> String {
    if let Some(rest) = fragment.strip_prefix('[') {
        return format!("{}[]", fragment_to_java_name(rest));
    }
    match fragment {
        "Z" => "boolean".to_string(),
        "B" => "byte".to_string(),
        "C" => "char".to_string(),
        "S" => "short".to_string(),
        "I" => "int".to_string(),
        "J" => "long".to_string(),
        "F" => "float".to_string(),
        "D" => "double".to_string(),
        "V" => "void".to_string(),
        _ => fragment
            .strip_prefix('L')
            .and_then(|s| s.strip_suffix(';'))
            .unwrap_or(fragment)
            .rsplit('/')
            .next()
            .unwrap_or(fragment)
            .to_string(),
    }
}

/// Render a method's parameter fragment run (e.g. `ILjava/lang/String;`) as
/// Java-source-style argument names (`int, String`). Only used for the
/// human-readable operation string, so malformed input degrades gracefully
/// (a fragment that cannot be split is skipped rather than looping).
fn arg_java_names(fragments: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = fragments;
    while !rest.is_empty() {
        // One fragment is a type token, optionally prefixed by array
        // brackets: `I`, `Ljava/lang/String;`, `[I`, `[[Ljava/lang/String;`.
        let mut end = 0;
        while rest[end..].starts_with('[') {
            end += 1;
        }
        let token_len = match rest[end..].chars().next() {
            Some('L') => rest[end..].find(';').map(|i| i + 1).unwrap_or(rest.len() - end),
            Some(_) => 1,
            // Defensive: a malformed tail makes no progress — stop rather
            // than loop forever.
            None => break,
        };
        end += token_len;
        names.push(fragment_to_java_name(&rest[..end]));
        rest = &rest[end..];
    }
    names
}

/// The dotted binary name of a class (`java.lang.Integer`), read best-effort
/// on the error path: when the JNI reads fail, `None` lets the operation
/// string fall back to the class-less form.
fn class_global_name(env: &mut Env<'_>, class: &Global<JClass<'static>>) -> Option<String> {
    let local: JClass = env.new_local_ref(class).ok()?;
    class_name(env, &local).ok()
}

/// The dotted binary name of an object's runtime class, best-effort.
fn object_class_name(env: &mut Env<'_>, obj: &Global<JObject<'static>>) -> Option<String> {
    let local = env.new_local_ref(obj).ok()?;
    let class = get_object_class(env, &local).ok()?;
    class_name(env, &class).ok()
}

/// The operation string for an instance-method call, e.g.
/// `calling append(String) on java.lang.StringBuilder`. The class is the
/// object's runtime class, resolved best-effort: when the JNI reads fail
/// the operation still names the method and its argument types.
fn instance_call_operation(
    env: &mut Env<'_>,
    obj: &Global<JObject<'static>>,
    name: &str,
    args_fragments: &str,
) -> String {
    let args = arg_java_names(args_fragments).join(", ");
    match object_class_name(env, obj) {
        Some(class) => format!("calling {name}({args}) on {class}"),
        None => format!("calling {name}({args})"),
    }
}

/// The operation string for a static-method call, e.g.
/// `calling parseInt(String) on java.lang.Integer`.
fn static_call_operation(
    env: &mut Env<'_>,
    class: &Global<JClass<'static>>,
    name: &str,
    args_fragments: &str,
) -> String {
    let args = arg_java_names(args_fragments).join(", ");
    match class_global_name(env, class) {
        Some(class) => format!("calling {name}({args}) on {class}"),
        None => format!("calling {name}({args})"),
    }
}

/// The operation string for a constructor call, e.g.
/// `constructing java.lang.Integer(String)`.
fn constructor_operation(
    env: &mut Env<'_>,
    class: &Global<JClass<'static>>,
    args_fragments: &str,
) -> String {
    let args = arg_java_names(args_fragments).join(", ");
    match class_global_name(env, class) {
        Some(class) => format!("constructing {class}({args})"),
        None => format!("constructing <unknown class>({args})"),
    }
}

/// The operation string for a field access, e.g.
/// `reading field base on com.example.NativeLib` /
/// `writing static field MAGIC on com.example.Kit`.
fn field_operation(
    env: &mut Env<'_>,
    verb: &str,
    name: &str,
    is_static: bool,
    class: impl FnOnce(&mut Env<'_>) -> Option<String>,
) -> String {
    let kind = if is_static { "static field" } else { "field" };
    match class(env) {
        Some(class) => format!("{verb} {kind} {name} on {class}"),
        None => format!("{verb} {kind} {name}"),
    }
}

/// Attach the operation context to a failed dynamic call.
///
/// Two errors are passed through unwrapped:
///
/// * one that is already a [`JavaError::WithContext`] — the wrap is
///   idempotent, a failure never accumulates a stack of operation contexts;
/// * a raw `Jni(FieldNotFound)` — the `bind!` bool-field accessor probes
///   for that exact shape to fall back to bean-style accessor methods.
fn operation_context(
    env: &mut Env<'_>,
    err: JavaError,
    operation: impl FnOnce(&mut Env<'_>) -> String,
) -> JavaError {
    match err {
        JavaError::WithContext { .. } => err,
        JavaError::Jni(jni::errors::Error::FieldNotFound { .. }) => err,
        other => other.with_operation(operation(env)),
    }
}

// ---------------------------------------------------------------------------
// Instance methods / fields
// ---------------------------------------------------------------------------

/// Call an instance method; `R` is chosen by the caller's annotation.
///
/// The signature built from the annotation is tried first; if the JVM reports
/// the method as not found (which happens whenever the annotation's return
/// fragment is not the exact one, e.g. `JObject` or `()`), the exact
/// signature is resolved via reflection and the call is retried.
pub(crate) fn call_method<'env, A: ToJava, R: FromJava>(
    env: &mut Env<'env>,
    obj: &Global<JObject<'static>>,
    name: &str,
    args: &A,
) -> JavaResult<R> {
    let args_fragments = args.java_args();
    let result = (|| {
        let name_j = JNIString::from(name);
        let rms = parse_method_sig(&args_fragments, &R::java_return_type())?;
        let sig: MethodSignature = (&rms).into();
        let arg_list = args.to_java(env)?;

        let result = {
            let jvalues: Vec<JValue> = arg_list.iter().map(to_jvalue).collect();
            with_check(env, |env| env.call_method(obj, name_j.clone(), sig, &jvalues))
        };
        match result {
            Ok(v) => {
                let r = R::from_java(env, v);
                finish(env, r)
            }
            Err(JavaError::Jni(jni::errors::Error::MethodNotFound { .. })) => {
                let obj_local = env.new_local_ref(obj)?;
                let class = get_object_class(env, &obj_local)?;
                let (rms, adjusted) = resolve_exact_method_sig(env, &class, name, arg_list, false)?;
                let sig: MethodSignature = (&rms).into();
                let jvalues: Vec<JValue> = adjusted.iter().map(to_jvalue).collect();
                let result = with_check(env, |env| env.call_method(obj, name_j, sig, &jvalues))?;
                let r = R::from_java(env, result);
                finish(env, r)
            }
            Err(e) => Err(e),
        }
    })();
    result.map_err(|err| {
        operation_context(env, err, |env| instance_call_operation(env, obj, name, &args_fragments))
    })
}

/// The JNI type fragment of a raw [`JavaArg`], used when a signature must be
/// derived from the argument *values* (the bean mapping's getter/setter
/// calls, feature `serde`) rather than from Rust types via `ToJava::java_args`.
#[cfg(feature = "serde")]
fn java_arg_fragment(arg: &JavaArg<'_>) -> String {
    match arg {
        JavaArg::Bool(_) => "Z".to_string(),
        JavaArg::Byte(_) => "B".to_string(),
        JavaArg::Char(_) => "C".to_string(),
        JavaArg::Short(_) => "S".to_string(),
        JavaArg::Int(_) => "I".to_string(),
        JavaArg::Long(_) => "J".to_string(),
        JavaArg::Float(_) => "F".to_string(),
        JavaArg::Double(_) => "D".to_string(),
        // Object arguments are deliberately generic: the bean mapping does
        // not know the declared parameter type at compile time, so the
        // reflection fallback must resolve it anyway.
        JavaArg::Object(_) => "Ljava/lang/Object;".to_string(),
    }
}

/// Call an instance method whose **exact signature is unknown at compile
/// time**, returning the raw JNI value without a `FromJava` conversion.
///
/// This is the raw-value analog of [`call_method`], used by the bean mapping
/// (getter/setter reflection): the parameter fragments are derived from the
/// [`JavaArg`] list and the return fragment is the deliberately-generic
/// `Ljava/lang/Object;`, so the first attempt fails on modern JVMs and the
/// reflection fallback (the same exact/box/unbox matching passes as
/// [`call_method`]) resolves the exact signature and retries. The raw
/// `JValueOwned` result is returned untouched — a primitive getter return
/// (e.g. `long`) stays visible to the caller, where [`call_method`] would
/// reject it (its `R: FromJava` conversion for `JObject` requires an object
/// value).
///
/// When no method named `name` accepting `args` exists, the reflection
/// fallback's `could not resolve` error (an [`JavaError::InvalidArgument`])
/// is returned; the bean mapping turns that into a property-naming error.
///
/// Only used by the bean mapping (`crate::bean`, feature `serde`), so it is
/// compiled out of feature-less builds.
#[cfg(feature = "serde")]
pub(crate) fn call_method_raw<'env>(
    env: &mut Env<'env>,
    obj: &Global<JObject<'static>>,
    name: &str,
    args: Vec<JavaArg<'env>>,
) -> JavaResult<JValueOwned<'env>> {
    let name_j = JNIString::from(name);
    let params: String = args.iter().map(java_arg_fragment).collect();
    let rms = parse_method_sig(&params, "Ljava/lang/Object;")?;
    let sig: MethodSignature = (&rms).into();

    let result = {
        let jvalues: Vec<JValue> = args.iter().map(to_jvalue).collect();
        with_check(env, |env| env.call_method(obj, name_j.clone(), sig, &jvalues))
    };
    match result {
        Ok(v) => finish(env, Ok(v)),
        Err(JavaError::Jni(jni::errors::Error::MethodNotFound { .. })) => {
            let obj_local = env.new_local_ref(obj)?;
            let class = get_object_class(env, &obj_local)?;
            let (rms, adjusted) = resolve_exact_method_sig(env, &class, name, args, false)?;
            let sig: MethodSignature = (&rms).into();
            let jvalues: Vec<JValue> = adjusted.iter().map(to_jvalue).collect();
            let result = with_check(env, |env| env.call_method(obj, name_j, sig, &jvalues))?;
            finish(env, Ok(result))
        }
        Err(e) => Err(e),
    }
}

/// Construct an object whose **exact constructor signature is unknown at
/// compile time**, from raw [`JavaArg`] values.
///
/// This is the raw-value analog of [`new_object`], used by the bean mapping
/// to build a Java **record** through its canonical constructor (feature
/// `serde`): the parameter fragments are derived from the [`JavaArg`] list
/// (object arguments are deliberately generic), so the first attempt fails
/// on modern JVMs and the reflection fallback (the same exact/box/unbox
/// matching passes as [`new_object`]) resolves the exact constructor and
/// retries.
///
/// Only used by the bean mapping (`crate::bean`, feature `serde`), so it is
/// compiled out of feature-less builds.
#[cfg(feature = "serde")]
pub(crate) fn new_object_raw<'env>(
    env: &mut Env<'env>,
    class: &Global<JClass<'static>>,
    args: Vec<JavaArg<'env>>,
) -> JavaResult<JObjectHandle> {
    let params: String = args.iter().map(java_arg_fragment).collect();
    let rms = parse_method_sig(&params, "V")?;
    let sig: MethodSignature = (&rms).into();

    let result = {
        let jvalues: Vec<JValue> = args.iter().map(to_jvalue).collect();
        with_check(env, |env| env.new_object(class, sig, &jvalues))
    };
    match result {
        Ok(obj) => finish(env, Ok(JObjectHandle::from_global(env.new_global_ref(obj)?))),
        Err(JavaError::Jni(jni::errors::Error::MethodNotFound { .. })) => {
            let class_local: JClass = env.new_local_ref(class)?;
            let (rms, adjusted) = resolve_exact_ctor_sig(env, &class_local, args)?;
            let sig: MethodSignature = (&rms).into();
            let jvalues: Vec<JValue> = adjusted.iter().map(to_jvalue).collect();
            let obj = with_check(env, |env| env.new_object(class, sig, &jvalues))?;
            finish(env, Ok(JObjectHandle::from_global(env.new_global_ref(obj)?)))
        }
        Err(e) => Err(e),
    }
}

/// Call a static method; `R` is chosen by the caller's annotation.
///
/// See [`call_method`] for the reflection fallback that kicks in when the
/// annotation's return fragment is not the exact one.
pub(crate) fn call_static_method<'env, A: ToJava, R: FromJava>(
    env: &mut Env<'env>,
    class: &Global<JClass<'static>>,
    name: &str,
    args: &A,
) -> JavaResult<R> {
    let args_fragments = args.java_args();
    let result = (|| {
        let name_j = JNIString::from(name);
        let rms = parse_method_sig(&args_fragments, &R::java_return_type())?;
        let sig: MethodSignature = (&rms).into();
        let arg_list = args.to_java(env)?;

        let result = {
            let jvalues: Vec<JValue> = arg_list.iter().map(to_jvalue).collect();
            with_check(env, |env| env.call_static_method(class, name_j.clone(), sig, &jvalues))
        };
        match result {
            Ok(v) => {
                let r = R::from_java(env, v);
                finish(env, r)
            }
            Err(JavaError::Jni(jni::errors::Error::MethodNotFound { .. })) => {
                let class_local: JClass = env.new_local_ref(class)?;
                let (rms, adjusted) = resolve_exact_method_sig(env, &class_local, name, arg_list, true)?;
                let sig: MethodSignature = (&rms).into();
                let jvalues: Vec<JValue> = adjusted.iter().map(to_jvalue).collect();
                let result =
                    with_check(env, |env| env.call_static_method(class, name_j, sig, &jvalues))?;
                let r = R::from_java(env, result);
                finish(env, r)
            }
            Err(e) => Err(e),
        }
    })();
    result.map_err(|err| {
        operation_context(env, err, |env| static_call_operation(env, class, name, &args_fragments))
    })
}

/// Construct a new object; `A` is the constructor argument list.
///
/// The signature built from the annotation is tried first; if the JVM reports
/// the constructor as not found (which happens whenever the annotation's
/// argument types are not the exact parameter types, e.g. a `String` for a
/// `WeakReference(Object)` constructor, a primitive for an `Object`-typed
/// parameter, or a wrapper object for a primitive parameter), the exact
/// signature is resolved via reflection
/// (`Class.getConstructors()`, with the same three matching passes as
/// [`call_method`]) and the construction is retried.
pub(crate) fn new_object<'env, A: ToJava>(
    env: &mut Env<'env>,
    class: &Global<JClass<'static>>,
    args: &A,
) -> JavaResult<JObjectHandle> {
    let args_fragments = args.java_args();
    let result = (|| {
        let rms = parse_method_sig(&args_fragments, "V")?;
        let sig: MethodSignature = (&rms).into();
        let arg_list = args.to_java(env)?;

        let obj = {
            let jvalues: Vec<JValue> = arg_list.iter().map(to_jvalue).collect();
            match with_check(env, |env| env.new_object(class, sig, &jvalues)) {
                Ok(obj) => obj,
                Err(JavaError::Jni(jni::errors::Error::MethodNotFound { .. })) => {
                    let class_local: JClass = env.new_local_ref(class)?;
                    let (rms, adjusted) = resolve_exact_ctor_sig(env, &class_local, arg_list)?;
                    let sig: MethodSignature = (&rms).into();
                    let jvalues: Vec<JValue> = adjusted.iter().map(to_jvalue).collect();
                    with_check(env, |env| env.new_object(class, sig, &jvalues))?
                }
                Err(e) => return Err(e),
            }
        };
        finish(env, Ok(JObjectHandle::from_global(env.new_global_ref(obj)?)))
    })();
    result.map_err(|err| {
        operation_context(env, err, |env| constructor_operation(env, class, &args_fragments))
    })
}

/// Read an instance field; `F` must be the exact Java field type.
pub(crate) fn get_field<'env, F: FromJava>(
    env: &mut Env<'env>,
    obj: &Global<JObject<'static>>,
    name: &str,
) -> JavaResult<F> {
    let result = (|| {
        let name_j = JNIString::from(name);
        let rfs = parse_field_sig(&F::java_return_type())?;
        let sig = rfs.field_signature();
        let result = with_check(env, |env| env.get_field(obj, name_j, sig))?;
        let r = F::from_java(env, result);
        finish(env, r)
    })();
    result.map_err(|err| {
        operation_context(env, err, |env| {
            field_operation(env, "reading", name, false, |env| object_class_name(env, obj))
        })
    })
}

/// Write an instance field; the Java type is derived from `V`.
pub(crate) fn set_field<'env, V: ToJava>(
    env: &mut Env<'env>,
    obj: &Global<JObject<'static>>,
    name: &str,
    value: &V,
) -> JavaResult<()> {
    let result = (|| {
        let name_j = JNIString::from(name);
        let rfs = parse_field_sig(&value.java_args())?;
        let sig = rfs.field_signature();
        let arg_list = value.to_java(env)?;
        let jvalue = to_jvalue(
            arg_list
                .first()
                .ok_or_else(|| JavaError::InvalidArgument("a field value must be a single value"))?,
        );
        with_check(env, |env| env.set_field(obj, name_j, sig, jvalue))?;
        Ok(())
    })();
    result.map_err(|err| {
        operation_context(env, err, |env| {
            field_operation(env, "writing", name, false, |env| object_class_name(env, obj))
        })
    })
}

/// Read a static field; `F` must be the exact Java field type.
pub(crate) fn get_static_field<'env, F: FromJava>(
    env: &mut Env<'env>,
    class: &Global<JClass<'static>>,
    name: &str,
) -> JavaResult<F> {
    let result = (|| {
        let name_j = JNIString::from(name);
        let rfs = parse_field_sig(&F::java_return_type())?;
        let sig = rfs.field_signature();
        let result = with_check(env, |env| env.get_static_field(class, name_j, sig))?;
        let r = F::from_java(env, result);
        finish(env, r)
    })();
    result.map_err(|err| {
        operation_context(env, err, |env| {
            field_operation(env, "reading", name, true, |env| class_global_name(env, class))
        })
    })
}

/// Write a static field; the Java type is derived from `V`.
pub(crate) fn set_static_field<'env, V: ToJava>(
    env: &mut Env<'env>,
    class: &Global<JClass<'static>>,
    name: &str,
    value: &V,
) -> JavaResult<()> {
    let result = (|| {
        let name_j = JNIString::from(name);
        let rfs = parse_field_sig(&value.java_args())?;
        let sig = rfs.field_signature();
        let arg_list = value.to_java(env)?;
        let jvalue = to_jvalue(
            arg_list
                .first()
                .ok_or_else(|| JavaError::InvalidArgument("a field value must be a single value"))?,
        );
        with_check(env, |env| env.set_static_field(class, name_j, sig, jvalue))?;
        Ok(())
    })();
    result.map_err(|err| {
        operation_context(env, err, |env| {
            field_operation(env, "writing", name, true, |env| class_global_name(env, class))
        })
    })
}

// ---------------------------------------------------------------------------
// Arrays
// ---------------------------------------------------------------------------

/// The length of a primitive array.
pub(crate) fn array_len_kind<'env>(
    env: &mut Env<'env>,
    global: &Global<JObject<'static>>,
    kind: ArrayKind,
) -> JavaResult<usize> {
    let local = env.new_local_ref(global)?;
    macro_rules! len_of {
        ($arr:ident) => {{
            let arr: jni::objects::$arr<'env> = jni::objects::$arr::cast_local(env, local)?;
            with_check(env, |env| arr.len(env))?
        }};
    }
    Ok(match kind {
        ArrayKind::Bool => len_of!(JBooleanArray),
        ArrayKind::Byte => len_of!(JByteArray),
        ArrayKind::Char => len_of!(JCharArray),
        ArrayKind::Short => len_of!(JShortArray),
        ArrayKind::Int => len_of!(JIntArray),
        ArrayKind::Long => len_of!(JLongArray),
        ArrayKind::Float => len_of!(JFloatArray),
        ArrayKind::Double => len_of!(JDoubleArray),
    })
}

/// The length of an object array.
pub(crate) fn array_len_object<'env>(
    env: &mut Env<'env>,
    global: &Global<JObject<'static>>,
) -> JavaResult<usize> {
    let local = env.new_local_ref(global)?;
    let arr: jni::objects::JObjectArray<'env> = jni::objects::JObjectArray::<JObject>::cast_local(env, local)?;
    with_check(env, |env| arr.len(env))
}

/// Read one element of a primitive array as a raw `JValueOwned`.
pub(crate) fn array_get<'env>(
    env: &mut Env<'env>,
    global: &Global<JObject<'static>>,
    index: usize,
    kind: ArrayKind,
) -> JavaResult<JValueOwned<'env>> {
    let local = env.new_local_ref(global)?;
    let idx = index as i32;
    match kind {
        ArrayKind::Bool => {
            let arr: jni::objects::JBooleanArray<'env> = jni::objects::JBooleanArray::cast_local(env, local)?;
            let mut buf = [false];
            with_check(env, |env| arr.get_region(env, idx, &mut buf))?;
            Ok(JValueOwned::Bool(buf[0]))
        }
        ArrayKind::Byte => {
            let arr: jni::objects::JByteArray<'env> = jni::objects::JByteArray::cast_local(env, local)?;
            let mut buf = [0i8];
            with_check(env, |env| arr.get_region(env, idx, &mut buf))?;
            Ok(JValueOwned::Byte(buf[0]))
        }
        ArrayKind::Char => {
            let arr: jni::objects::JCharArray<'env> = jni::objects::JCharArray::cast_local(env, local)?;
            let mut buf = [0u16];
            with_check(env, |env| arr.get_region(env, idx, &mut buf))?;
            Ok(JValueOwned::Char(buf[0]))
        }
        ArrayKind::Short => {
            let arr: jni::objects::JShortArray<'env> = jni::objects::JShortArray::cast_local(env, local)?;
            let mut buf = [0i16];
            with_check(env, |env| arr.get_region(env, idx, &mut buf))?;
            Ok(JValueOwned::Short(buf[0]))
        }
        ArrayKind::Int => {
            let arr: jni::objects::JIntArray<'env> = jni::objects::JIntArray::cast_local(env, local)?;
            let mut buf = [0i32];
            with_check(env, |env| arr.get_region(env, idx, &mut buf))?;
            Ok(JValueOwned::Int(buf[0]))
        }
        ArrayKind::Long => {
            let arr: jni::objects::JLongArray<'env> = jni::objects::JLongArray::cast_local(env, local)?;
            let mut buf = [0i64];
            with_check(env, |env| arr.get_region(env, idx, &mut buf))?;
            Ok(JValueOwned::Long(buf[0]))
        }
        ArrayKind::Float => {
            let arr: jni::objects::JFloatArray<'env> = jni::objects::JFloatArray::cast_local(env, local)?;
            let mut buf = [0f32];
            with_check(env, |env| arr.get_region(env, idx, &mut buf))?;
            Ok(JValueOwned::Float(buf[0]))
        }
        ArrayKind::Double => {
            let arr: jni::objects::JDoubleArray<'env> = jni::objects::JDoubleArray::cast_local(env, local)?;
            let mut buf = [0f64];
            with_check(env, |env| arr.get_region(env, idx, &mut buf))?;
            Ok(JValueOwned::Double(buf[0]))
        }
    }
}

/// Write one element of a primitive array.
pub(crate) fn array_set<'env>(
    env: &mut Env<'env>,
    global: &Global<JObject<'static>>,
    index: usize,
    arg: &JavaArg<'env>,
    kind: ArrayKind,
) -> JavaResult<()> {
    let local = env.new_local_ref(global)?;
    let idx = index as i32;
    macro_rules! set_one {
        ($arr:ident, $variant:ident, $expected:literal) => {{
            let arr: jni::objects::$arr<'env> = jni::objects::$arr::cast_local(env, local)?;
            match arg {
                JavaArg::$variant(v) => {
                    with_check(env, |env| arr.set_region(env, idx, &[*v]))?;
                    Ok(())
                }
                _ => Err(JavaError::InvalidArgument($expected)),
            }
        }};
    }
    match kind {
        ArrayKind::Bool => set_one!(JBooleanArray, Bool, "expected a boolean array element"),
        ArrayKind::Byte => set_one!(JByteArray, Byte, "expected a byte array element"),
        ArrayKind::Char => set_one!(JCharArray, Char, "expected a char array element"),
        ArrayKind::Short => set_one!(JShortArray, Short, "expected a short array element"),
        ArrayKind::Int => set_one!(JIntArray, Int, "expected an int array element"),
        ArrayKind::Long => set_one!(JLongArray, Long, "expected a long array element"),
        ArrayKind::Float => set_one!(JFloatArray, Float, "expected a float array element"),
        ArrayKind::Double => set_one!(JDoubleArray, Double, "expected a double array element"),
    }
}

/// Read one element of an object array; `None` for `null` elements.
pub(crate) fn array_get_object<'env>(
    env: &mut Env<'env>,
    global: &Global<JObject<'static>>,
    index: usize,
) -> JavaResult<Option<JObject<'env>>> {
    let local = env.new_local_ref(global)?;
    let arr: jni::objects::JObjectArray<'env> = jni::objects::JObjectArray::<JObject>::cast_local(env, local)?;
    array_get_object_local(env, &arr, index)
}

/// Read one element of an already-cast object array; `None` for `null`.
pub(crate) fn array_get_object_local<'env>(
    env: &mut Env<'env>,
    arr: &jni::objects::JObjectArray<'env>,
    index: usize,
) -> JavaResult<Option<JObject<'env>>> {
    let e = with_check(env, |env| arr.get_element(env, index))?;
    Ok((!e.is_null()).then_some(e))
}

/// Write one element of an object array.
pub(crate) fn array_set_object<'env>(
    env: &mut Env<'env>,
    global: &Global<JObject<'static>>,
    index: usize,
    value: &Arc<Global<JObject<'static>>>,
) -> JavaResult<()> {
    let local = env.new_local_ref(global)?;
    let arr: jni::objects::JObjectArray<'env> = jni::objects::JObjectArray::<JObject>::cast_local(env, local)?;
    let vlocal = env.new_local_ref(&**value)?;
    with_check(env, |env| arr.set_element(env, index, &vlocal))?;
    Ok(())
}
