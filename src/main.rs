// Interdit tout code `unsafe` dans notre crate : le compilateur refusera de compiler
// s'il en trouve. Les garanties mémoire de Rust s'appliquent donc à tout notre code.
#![forbid(unsafe_code)]

fn main() {
    println!("Evolve TV");
}
