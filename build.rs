fn main() {
    // esp-hal requirements
    println!("cargo:rustc-link-arg-bins=-Tlinkall.x");

    // defmt requirements
    println!("cargo:rustc-link-arg=-Tdefmt.x");
}
