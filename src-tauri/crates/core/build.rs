//! Compiles the C shim over the vendored Signalsmith Stretch headers, for the
//! `playback` feature. Its FFI is declared by hand in `playback/stretch`, so
//! nothing here needs libclang, which the Flatpak SDK does not ship.

fn main() {
    #[cfg(feature = "playback")]
    {
        println!("cargo:rerun-if-changed=src/playback/stretch/shim.cpp");
        println!("cargo:rerun-if-changed=vendor/signalsmith-stretch");
        let mut build = cc::Build::new();
        build
            .cpp(true)
            .std("c++14")
            .file("src/playback/stretch/shim.cpp")
            .include("vendor/signalsmith-stretch")
            // Whatever the profile: left unoptimized, these templates cost a
            // debug build a good part of a core, on the decoder thread.
            .opt_level(3)
            .warnings(false);
        // The Windows app links its C runtime statically and needs no
        // redistributable. Nothing else in it is C++, and `std::vector` would
        // make it import MSVCP140.dll for one function, without which it would
        // not start. The shim defines that function, and this keeps the header
        // from declaring it imported.
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            build.define("_CRTIMP2_PURE", Some(""));
        }
        build.compile("waveflow_stretch");
    }
}
