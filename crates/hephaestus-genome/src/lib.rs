//! Immutable Genome and World compilation.

mod compiler;
mod error;
mod genome;
mod registry;
mod world;

pub use compiler::SourceFormat;
pub use error::CompileError;
pub use genome::{CompiledGenome, compile_genome};
pub use registry::{
    GenomeRecord, RegisteredGenome, RegisteredObjects, RegisteredWorld, RegistrationError,
    RegistrationKind, WorldRecord,
};
pub use world::{CompiledWorld, WorldEvaluationPolicy, compile_world, ensure_comparable};
