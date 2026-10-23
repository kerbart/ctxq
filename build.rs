// Exposes the target triple to `documentum --version`; CI injects DOCUMENTUM_VERSION itself.
fn main() {
    println!("cargo:rerun-if-env-changed=DOCUMENTUM_VERSION");
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=DOCUMENTUM_TARGET={target}");
    }
}
