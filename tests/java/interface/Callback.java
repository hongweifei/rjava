/**
 * Fixture for the `interface` integration tests (tests/interface.rs): a
 * plain Java interface that rjava implements from Rust via
 * `java.lang.reflect.Proxy` — no Java implementation class anywhere.
 *
 * Deliberately covers all the interesting shapes:
 *
 * <ul>
 *   <li>a reference-returning method with a reference parameter
 *       (`greet`),</li>
 *   <li>a primitive-returning method with primitive parameters (`add`,
 *       whose result the proxy's generated code unboxes from the returned
 *       Integer),</li>
 *   <li>an <b>overloaded</b> pair (`add(int, int)` + `add(long, long)`) —
 *       the Rust handler tells them apart via {@code call.param_types},</li>
 *   <li>a {@code void} method (`ping`),</li>
 *   <li>a {@code default} method with a known implementation (`shout`) —
 *       rjava auto-forwards it to this default body instead of the
 *       handler,</li>
 *   <li>a domain method named like an {@code Object} method but with a
 *       different signature (`toString(int)`) — it must reach the handler,
 *       proving the library's {@code toString()} interception is
 *       signature-based.</li>
 * </ul>
 */
public interface Callback {
    String greet(String name);

    int add(int a, int b);

    long add(long a, long b);

    void ping();

    /** `String[]` → `String`, or `null` for the empty array — the typed
     *  `interface!` `Vec<String>` → `Option<String>` round trip. */
    String apply(String[] words);

    /** A method the typed `interface!` trait deliberately does *not*
     *  declare — calling it must produce the "no declared method" error. */
    String mystery(String s);

    /**
     * A {@code default} method with a known implementation:
     * {@code shout("rust")} returns {@code "RUST"}.
     */
    default String shout(String s) {
        return s.toUpperCase();
    }

    /** A domain overload of `toString` with a parameter — not the
     *  `Object` method, so it reaches the Rust handler. */
    String toString(int n);
}
