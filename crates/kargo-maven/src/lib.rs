//! Maven repository support: coordinates, versions, POMs, Gradle module
//! metadata, downloading and transitive resolution.

pub mod central;
pub mod coord;
pub mod deploy;
pub mod module;
pub mod pom;
pub mod repo;
pub mod resolver;
pub mod snapshot;
mod suggest;
#[cfg(test)]
mod testserver;
pub mod version;

pub use coord::{Artifact, CoordSpec, ModuleId};
pub use module::Platform;
pub use repo::{MavenClient, Repository};
pub use resolver::{DependencyRequest, Exclusion, Explanation, Resolution, Resolver};
pub use version::{Version, VersionReq, select_latest};

/// Threads for network-bound work (metadata and artifact downloads), independent of the CPU count.
const IO_THREADS: usize = 16;

/// The thread pool for downloads: large enough to keep many requests in flight.
pub fn io_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        rayon::ThreadPoolBuilder::new()
            .num_threads(IO_THREADS)
            .thread_name(|i| format!("kargo-io-{i}"))
            .build()
            .expect("failed to start the download thread pool")
    })
}

/// Maps `f` over `items` on the download pool, keeping item order.
pub fn par_map_io<T: Sync, R: Send>(items: &[T], f: impl Fn(&T) -> R + Sync + Send) -> Vec<R> {
    use rayon::prelude::*;
    io_pool().install(|| items.par_iter().map(f).collect())
}
