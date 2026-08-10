package bean;

/**
 * A plain Java bean nested inside {@link Contact} for the bean-to-bean
 * nesting tests (tests/bean_map.rs): a {@code street} String property plus a
 * nested {@link City} bean property, with public getters/setters and a
 * public no-arg constructor. The Rust-side field is typed
 * {@code JavaBean<City>}, so writing must build a {@code bean.City} object
 * and call {@code setCity(City)}; reading must call {@code getCity()} and
 * read the returned object's properties through its own getters.
 */
public class Address {
    private String street;
    private City city;

    public Address() {}

    public String getStreet() {
        return street;
    }

    public void setStreet(String street) {
        this.street = street;
    }

    public City getCity() {
        return city;
    }

    public void setCity(City city) {
        this.city = city;
    }

    /** Test hook: returns the same instance. */
    public static Address echo(Address a) {
        return a;
    }
}
