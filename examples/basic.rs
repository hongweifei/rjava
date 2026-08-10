//! End-to-end demo: build a JVM, call constructors/methods/statics, push and
//! pop from an `ArrayList`, watch an exception become a typed error, and play
//! with arrays.
//!
//! Run with: `cargo run --example basic`

use rjava::prelude::*;

fn main() -> JavaResult<()> {
    // 1) JVM facade — creates the JVM via the invocation API (reuses an
    //    existing JVM if one is already present).
    let java = Java::builder().option("-Xmx128m").build()?;
    println!("JVM started");

    // 2) Classes & objects — the StringBuilder dance.
    let sb_class = java.class("java.lang.StringBuilder")?;
    let sb = sb_class.new_instance(("Hello",))?;
    let len: i32 = sb.call("length", ())?;
    println!("StringBuilder length: {len}");
    sb.call_void("append", (" world",))?;
    sb.call_void("append", (42_i32,))?;
    let text: String = sb.call("toString", ())?;
    println!("StringBuilder text:   {text}");
    let runtime_class = sb.class()?;
    println!("Runtime class:        {}", runtime_class.name()?);

    // 3) Static members.
    let math = java.class("java.lang.Math")?;
    let max: i32 = math.call_static("max", (3_i32, 7_i32))?;
    let pi: f64 = math.get_static_field("PI")?;
    println!("max(3, 7) = {max}, PI = {pi}");

    // 4) Collections: push/pop an ArrayList<Integer>.
    // Primitives are boxed automatically for Object-typed parameters
    // (ArrayList.add(Object)), so plain ints work directly.
    let list = java.new_object("java.util.ArrayList", ())?;
    list.call_void("add", (10_i32,))?;
    list.call_void("add", (20_i32,))?;
    list.call_void("add", (30_i32,))?;
    let size: i32 = list.call("size", ())?;
    let first_obj: JObject = list.call("get", (0_i32,))?;
    let first: i32 = first_obj.call("intValue", ())?;
    println!("ArrayList: size={size}, first={first}");

    // 5) Exceptions become typed errors, with class + message captured.
    let integer = java.class("java.lang.Integer")?;
    match integer.call_static::<_, i32>("parseInt", ("not-a-number",)) {
        Ok(v) => println!("parseInt returned {v}"),
        Err(JavaError::JavaException { class, message }) => {
            println!("Caught Java exception: {class}: {message}");
        }
        Err(e) => println!("Unexpected error: {e}"),
    }

    // 6) Arrays: primitives and objects.
    let arr: JArray<i32> = java.new_array(5)?;
    arr.set(0, 42)?;
    arr.set(4, 7)?;
    let stream: JObject = java.class("java.util.Arrays")?.call_static("stream", (&arr,))?;
    let total: i32 = stream.call("sum", ())?;
    println!("int[] sum: {total}");

    let names = java.new_object_array("java.lang.String", 3)?;
    names.set(0, java.new_object("java.lang.String", ("Ada",))?)?;
    names.set(1, java.new_object("java.lang.String", ("Grace",))?)?;
    let first_name: String = names.get(0)?;
    let missing: Option<JObject> = names.get(2)?; // null element -> None
    println!("names[0] = {first_name}, names[2] = {missing:?}");

    // 7) Strings with non-ASCII content.
    let s = java.new_object("java.lang.String", ("你好，世界 🌍",))?;
    let upper: String = s.call("toUpperCase", ())?;
    println!("toUpperCase: {upper}");

    // 8) Threads: std::thread + automatic attachment.
    let handle = std::thread::spawn(move || -> JavaResult<i64> {
        let math = java.class("java.lang.Math")?;
        let sum: i64 = math.call_static("multiplyExact", (6_i64, 7_i64))?;
        Ok(sum)
    });
    println!("Thread result: {}", handle.join().unwrap()?);

    Ok(())
}
