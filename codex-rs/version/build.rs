fn main() {
    let version =
        std::env::var("CODE_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string());

    println!("cargo:rustc-env=CODE_VERSION={version}");
    println!("cargo:rerun-if-env-changed=CODE_VERSION");
}
