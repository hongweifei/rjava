package rjbench;

/**
 * Fixture for the rjava benchmark harness (benches/bench.rs): a minimal
 * native-method surface — a no-op, a binary add, and array-typed
 * conversions — used to measure the per-call overhead of the native-call
 * machinery, the userdata identity lookup path, and the Vec<T> array
 * conversions in both directions.
 */
public class BenchLib {
    public static native void nop();
    public static native int add(int a, int b);

    // Array conversions (the Rust impls return/take the Vec form, so the
    // call site exercises the caller-side ToJava/FromJava conversion).
    public static native int[] range(int n);       // caller: int[] -> Vec<i32>
    public static native long sum(int[] xs);       // caller: Vec<i32> -> int[]
    public static native String[] strings(int n);  // caller: String[] -> Vec<String>
    public static native long totalLen(String[] xs); // caller: Vec<String> -> String[]
}
