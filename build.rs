// Exposes the target triple to `ctxq --version`; CI injects CTXQ_VERSION itself.
fn main() {
    println!("cargo:rerun-if-env-changed=CTXQ_VERSION");
    if let Ok(target) = std::env::var("TARGET") {
        println!("cargo:rustc-env=CTXQ_TARGET={target}");
    }
}
