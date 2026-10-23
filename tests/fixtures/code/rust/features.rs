use crate::support::{helper, EngineConfig};

/// Runs one unit of work.
pub trait Worker {
    fn run(&self);
}

pub struct Engine;

/* an outer comment { /* nested // comment } */ still a comment */
impl Worker for Engine {
    fn run(&self) {
        helper();
        println!("{} // not a comment", r#"{ raw }"#);
    }
}

pub fn build<T: Into<EngineConfig>>(
    value: T,
    name: &'static str,
) -> Engine {
    let _brace = '}'; log::write(name);
    Engine::new(value.into())
}
