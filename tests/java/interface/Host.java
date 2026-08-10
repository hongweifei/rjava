/**
 * Fixture for the `interface` integration tests (tests/interface.rs): the
 * Java-side consumer that calls a Rust-implemented {@link Callback} — proof
 * that the proxy is usable as an ordinary Java object.
 *
 * {@link #tryGreet} additionally proves exception propagation: the Rust
 * handler throws an IllegalStateException, and Java code here catches it.
 * The remaining methods drive the library-level behaviors: default-method
 * auto-forwarding, the overloaded {@code add}, the reserved {@code Object}
 * methods ({@code toString}/{@code hashCode}/{@code equals}), and the
 * domain {@code toString(int)} overload.
 */
public class Host {

    /** Calls `greet` on the Rust-implemented callback. */
    public static String greet(Callback cb, String name) {
        return cb.greet(name);
    }

    /** Calls the `int` overload of `add` on the callback. */
    public static int add(Callback cb, int a, int b) {
        return cb.add(a, b);
    }

    /** Calls the `long` overload of `add` on the callback. */
    public static long addLong(Callback cb, long a, long b) {
        return cb.add(a, b);
    }

    /** Calls `ping` on the Rust-implemented callback. */
    public static void ping(Callback cb) {
        cb.ping();
    }

    /** Calls the `String[]`-based `apply` on the callback. */
    public static String applyWords(Callback cb, String[] words) {
        return cb.apply(words);
    }

    /** Calls `mystery` — a method the typed trait does not declare. */
    public static String mystery(Callback cb, String s) {
        return cb.mystery(s);
    }

    /** Calls `greet` and catches the Rust-thrown IllegalStateException. */
    public static String tryGreet(Callback cb, String name) {
        try {
            return cb.greet(name);
        } catch (IllegalStateException e) {
            return "caught:" + e.getMessage();
        }
    }

    /** Calls the interface's `default` method — the default implementation
     *  must run (auto-forwarded by the library), not the Rust handler. */
    public static String shout(Callback cb) {
        return cb.shout("rust");
    }

    /** The proxy's `Object.toString()` (library-reserved). */
    public static String toStringOf(Callback cb) {
        return cb.toString();
    }

    /** The proxy's `Object.hashCode()` (library-reserved). */
    public static int hashCodeOf(Callback cb) {
        return cb.hashCode();
    }

    /** The proxy's `Object.equals` against itself. */
    public static boolean equalsSelf(Callback cb) {
        return cb.equals(cb);
    }

    /** The proxy's `Object.equals` against another object. */
    public static boolean equalsOther(Callback cb, Object other) {
        return cb.equals(other);
    }

    /** The domain `toString(int)` overload — must reach the Rust handler,
     *  not the library's `toString()` interception. */
    public static String toStringInt(Callback cb, int n) {
        return cb.toString(n);
    }
}
