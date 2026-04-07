/// Trait for emitting events. Implemented by application-level event logs
/// and adapters that bridge between crate-specific event types.
pub trait EventEmitter<E>: Send + Sync {
    fn emit(&self, event: E);
}
