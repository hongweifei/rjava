package com.example;

/** Fixture for rjava's native-method integration tests. */
public class NativeLib {
    public static native int add(int a, int b);
    public static native String shout(String s);
    public static native double avg(double[] xs);
    public static native int[] range(int n);
    public static native void fail(String msg);
    public static native int javaAbs(int x);      // Rust impl calls back into Java: Math.abs
    public static native String nullOrValue(boolean give); // returns null when give == false
    public static native int boom();              // Rust impl panics
    public static native int arityMismatch(int a); // Rust impl takes two args (deliberate bug)

    // Beyond-8-parameter methods: exercise the extended per-arity trampolines
    // and the call-side tuple limit (10 / 20 / mixed types incl. long+double).
    public static native long many10(int a, int b, int c, int d, int e, int f, int g, int h, int i, int j);
    public static native long many20(int a, int b, int c, int d, int e, int f, int g, int h, int i, int j, int k, int l, int m, int n, int o, int p, int q, int r, int s, int t);
    public static native long manyMix(byte a, short b, int c, long d, float e, double f, boolean g, char h, String i, long j);

    // Type-derived registrations: closures (native! without a signature string).
    public static native int addClosure(int a, int b); // registered with a closure
    public static native int addOffset(int x);         // registered with a capturing closure

    // Concrete reference return types: the type-derived registration
    // annotates them generically (Vec<JObject> -> `[Ljava/lang/Object;`,
    // JObject -> `Ljava/lang/Object;`), and register_natives resolves the
    // exact return type via reflection at registration time.
    public static native String[] splitCSV(String s); // Rust splits on ','
    public static native Object identity(Object o);   // genuinely Object-typed

    // Reference-typed arrays: `Vec<String>` ⇄ `String[]` in both directions.
    public static native String joinCSV(String[] xs); // Rust joins with ','
    public static native String[] withNull();        // Rust returns {"a", null, "b"}

    public int base;

    public native int times(int factor);
    public native long timesLong(int factor); // same shape as times, distinct signature
}
