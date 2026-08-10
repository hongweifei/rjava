package bind;

/**
 * Fixture for the bind! integration tests.
 *
 * Deliberately uses distinct method names (Rust has no overloading, so a
 * bind! wrapper can declare each Java method only once), chainable setters,
 * statics, String[] round trips, null-tolerant Option returns, and a couple
 * of methods whose declared Rust type is deliberately more generic than the
 * real signature (echo/raw) so the tests can exercise the exact-signature
 * reflection fallback.
 */
public class BindKit {

    private String label;
    private long value;

    // Raw fields for the bind! `field` declarations: a String, an int, a
    // bool (read directly), and two boolean *properties* exposed only
    // through accessors (`ready` via isReady; `usable` via getUsable AND
    // isUsable, so the get-first order is observable).
    public String tag;
    public int count;
    public boolean active;
    private boolean readyFlag = true;
    private boolean usableFlag = true;

    public static int MAGIC = 7;

    public BindKit(String label, long value) {
        this.label = label;
        this.value = value;
    }

    public boolean isReady() {
        return readyFlag;
    }

    public boolean getUsable() {
        return usableFlag;
    }

    public boolean isUsable() {
        return !usableFlag;
    }

    /** The Java method behind a `[java_name = "compute"]` alias. */
    public int compute(int x) {
        return x * 2;
    }

    public String label() {
        return label;
    }

    public BindKit setLabel(String label) {
        this.label = label;
        return this;
    }

    public long value() {
        return value;
    }

    public BindKit setValue(long value) {
        this.value = value;
        return this;
    }

    public long add(long a, long b) {
        return a + b;
    }

    public static long staticAdd(long a, long b) {
        return a + b;
    }

    public static BindKit create(String label) {
        return new BindKit(label, 0);
    }

    public String[] splitWords(String s) {
        return s.split(" ");
    }

    public String joinWords(String[] words) {
        return String.join(" ", words);
    }

    public String nullableString(int mode) {
        return mode == 0 ? null : "present";
    }

    public String[] nullableArray(int mode) {
        return mode == 0 ? null : new String[] { "a", "b" };
    }

    /** The real parameter is String; bind! declares it as JObject. */
    public String echo(String s) {
        return s;
    }

    /** The real return type is String; bind! declares it as JObject. */
    public Object raw() {
        return "raw";
    }
}
