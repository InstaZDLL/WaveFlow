//! Compiles the C shim over the vendored Signalsmith Stretch headers, for the
//! `playback` feature. Its FFI is declared by hand in `playback/stretch`, so
//! nothing here needs libclang, which the Flatpak SDK does not ship.

fn main() {
    #[cfg(feature = "playback")]
    {
        println!("cargo:rerun-if-changed=src/playback/stretch/shim.cpp");
        println!("cargo:rerun-if-changed=vendor/signalsmith-stretch");
        cc::Build::new()
            .cpp(true)
            .std("c++14")
            .file("src/playback/stretch/shim.cpp")
            .include("vendor/signalsmith-stretch")
            // Whatever the profile: left unoptimized, these templates cost a
            // debug build a good part of a core, on the decoder thread.
            .opt_level(3)
            .warnings(false)
            .compile("waveflow_stretch");
    }
}
