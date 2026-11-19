fn main() {
    // Some musl-gcc specs inject a PT_INTERP even into static PIEs. The
    // standalone server must also start on hosts without the musl loader.
    if std::env::var_os("CARGO_FEATURE_SERVER").is_some() && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("musl") {
        println!("cargo:rustc-link-arg-bin=qccom-server=-Wl,--no-dynamic-linker");
    }
}
