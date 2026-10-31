fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=cpp/include");
    println!("cargo:rerun-if-changed=cpp/src");

    cc::Build::new()
        .cpp(true)
        .std("c++20")
        .include("cpp/include")
        .file("cpp/src/ffi.cpp")
        .flag_if_supported("-fno-exceptions")
        .flag_if_supported("-fno-rtti")
        .warnings(true)
        .extra_warnings(true)
        .compile("memweave_memory");
}
