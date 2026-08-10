package bean;

/**
 * The innermost bean of the nesting tests (tests/bean_map.rs): a single
 * {@code name} String property. Nested inside {@link Address} via a
 * {@code JavaBean<City>} Rust field to exercise recursive bean-to-bean
 * nesting.
 */
public class City {
    private String name;

    public City() {}

    public String getName() {
        return name;
    }

    public void setName(String name) {
        this.name = name;
    }

    /** Test hook: returns the same instance. */
    public static City echo(City c) {
        return c;
    }
}
