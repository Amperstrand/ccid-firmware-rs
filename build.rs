use std::env;
use std::fs;
use std::path::Path;

fn main() {
    // Copy memory.x to OUT_DIR for the linker
    let out_dir = env::var_os("OUT_DIR").unwrap();
    let memory_x_path = Path::new("memory.x");

    if memory_x_path.exists() {
        let out_memory_x = Path::new(&out_dir).join("memory.x");
        fs::copy(memory_x_path, out_memory_x).unwrap();
    }

    println!("cargo:rustc-link-search={}", out_dir.to_str().unwrap());
    println!("cargo:rerun-if-changed=memory.x");
    println!("cargo:rerun-if-changed=build.rs");
}
