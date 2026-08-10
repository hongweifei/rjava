package bean;

/**
 * A bean with a setter but **no getter** — the missing-getter error test
 * (tests/bean_map.rs) reads a struct with a {@code label} field from an
 * instance of this class and expects a loud error naming the property and
 * the attempted {@code getLabel} / {@code isLabel} accessors.
 */
public class WriteOnly {
    private String label;

    public WriteOnly() {}

    public void setLabel(String label) {
        this.label = label;
    }
    // deliberately no getLabel() / isLabel()
}
