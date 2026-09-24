fn main() {
    println!("cargo:rerun-if-changed=../../crates/relay-storage-sqlite/migrations");
}
