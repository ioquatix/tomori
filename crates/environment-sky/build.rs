// Released under the MIT License.
// Copyright, 2026, by Samuel Williams.

#[path = "src/cloud_noise.rs"]
mod cloud_noise;
#[path = "src/light_bake.rs"]
mod light_bake;
#[allow(dead_code)]
#[path = "src/mesh.rs"]
mod mesh;

fn main() {
    println!("cargo:rerun-if-changed=src/cloud_noise.rs");
    println!("cargo:rerun-if-changed=src/mesh.rs");
    println!("cargo:rerun-if-changed=src/light_bake.rs");
    let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    std::fs::write(output.join("cloud-noise.rgba"), cloud_noise::bake()).unwrap();
    std::fs::write(output.join("scenery-lightmap.rgba"), light_bake::bake()).unwrap();
}
