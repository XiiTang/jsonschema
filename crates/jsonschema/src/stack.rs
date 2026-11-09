//! Refusing a build or a validation run that would overflow the stack, and releasing a validator
//! without overflowing it.
//!
//! Building and validating recurse through references. A reference chain is not bounded by the
//! nesting of the schema document, and at validation the depth is the chain times the instance's
//! nesting, so the recursion checks the stack it has left instead of counting levels. Releasing
//! recurses through the nodes each node owns; nothing can be refused there, so it moves to a new
//! stack segment.

/// Stack kept free for the code between two checks and for reporting the refusal.
#[cfg(not(target_arch = "wasm32"))]
const RED_ZONE: usize = 256 * 1024;

/// The stack segment a release low on stack continues on.
pub(crate) const RELEASE_SEGMENT: usize = 1024 * 1024;

/// Whether descending further could overflow this stack.
///
/// `false` where the platform does not tell how much stack is left.
#[inline]
pub(crate) fn low() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        matches!(stacker::remaining_stack(), Some(left) if left < RED_ZONE)
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}
