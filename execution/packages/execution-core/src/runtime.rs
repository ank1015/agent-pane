use crate::{ExecutionHostDescriptor, FileSystem, ProcessRuntime};

/// Complete provider-neutral runtime exposed by one execution host.
pub trait ExecutionRuntime: Send + Sync {
    fn descriptor(&self) -> &ExecutionHostDescriptor;

    fn filesystem(&self) -> &dyn FileSystem;

    fn processes(&self) -> &dyn ProcessRuntime;
}
