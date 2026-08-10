package bean;

/**
 * A plain Java bean with a **bean-typed property** — {@code address} is a
 * nested {@link Address} — used by the bean-to-bean nesting tests
 * (tests/bean_map.rs). The Rust-side field is typed
 * {@code JavaBean<Address>}, so writing {@code bean.Contact} must build a
 * nested {@code bean.Address} object and call {@code setAddress(Address)};
 * reading must call {@code getAddress()} and read the nested object's
 * properties through its getters.
 */
public class Contact {
    private String name;
    private Address address;

    public Contact() {}

    public String getName() {
        return name;
    }

    public void setName(String name) {
        this.name = name;
    }

    public Address getAddress() {
        return address;
    }

    public void setAddress(Address address) {
        this.address = address;
    }

    /** Test hook: returns the same instance. */
    public static Contact echo(Contact c) {
        return c;
    }
}
