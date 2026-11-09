fn main() {
    // include_dir! doesn't notice new files on its own
    println!("cargo:rerun-if-changed=../gifs");
    println!("cargo:rerun-if-changed=../phone");
    tauri_build::build()
}
