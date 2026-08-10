package bean;

/**
 * A bean with a getter but **no setter** — the missing-setter error test
 * (tests/bean_map.rs) writes a struct with an {@code id} field into this
 * class and expects a loud error naming the property and {@code setId}.
 */
public class ReadOnly {
    private long id;

    public ReadOnly() {}

    public long getId() {
        return id;
    }
    // deliberately no setId(long)
}
