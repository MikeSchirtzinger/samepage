//! Stub `convergence-attrs` — a no-op shim of brevity's convergence-protocol
//! proc-macro attributes. Lets brevity-annotated types compile in a standalone
//! workspace without pulling in brevity's substrate registry.
//!
//! If/when this workspace needs to participate in brevity's convergence
//! protocol, swap this dep for the real `convergence-attrs` from brevity.

use proc_macro::TokenStream;

/// No-op stub of `#[substrate(name = "...", since = "...", domain = "...")]`.
/// Drops the attribute arguments and emits the annotated item unchanged.
#[proc_macro_attribute]
pub fn substrate(_args: TokenStream, input: TokenStream) -> TokenStream {
    input
}

/// No-op stub of `#[capability(...)]`. Drops arguments, emits item unchanged.
#[proc_macro_attribute]
pub fn capability(_args: TokenStream, input: TokenStream) -> TokenStream {
    input
}

/// No-op stub of `#[convergence_test(...)]`. Drops arguments, emits item unchanged.
#[proc_macro_attribute]
pub fn convergence_test(_args: TokenStream, input: TokenStream) -> TokenStream {
    input
}
