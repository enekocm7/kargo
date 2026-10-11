//! Maven repository support: coordinates, versions, POMs, Gradle module
//! metadata, downloading and transitive resolution.

pub mod coord;
pub mod module;
pub mod pom;
pub mod repo;
pub mod resolver;
pub mod version;

pub use coord::{Artifact, CoordSpec, ModuleId};
pub use module::Platform;
pub use repo::{MavenClient, Repository};
pub use resolver::{DependencyRequest, Exclusion, Resolution, Resolver};
pub use version::{Version, VersionReq};
