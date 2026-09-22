//! `export_capability!`: the WebAssembly exports of one capability.

/// Export one [`Capability`](crate::Capability) as the JSON functions a
/// capability artifact provides: `abi`, `descriptor`, `project`, `plan`,
/// `validate`, `cascade`, `digest`, `changes`, `legend`. Use it once, at the
/// root of the capability's `cdylib` crate:
///
/// ```ignore
/// samepage_capability::export_capability!(MyCapability, MyCapability::new());
/// ```
///
/// The capability is constructed once, on first call. Each export forwards to
/// [`crate::envelope`], which is where the calling convention is implemented
/// and tested.
#[macro_export]
macro_rules! export_capability {
    ($ty:ty, $make:expr) => {
        #[doc(hidden)]
        pub mod __samepage_capability_exports {
            use super::*;
            use $crate::__private::wasm_bindgen::prelude::wasm_bindgen;

            fn instance() -> &'static $ty {
                static CELL: ::std::sync::OnceLock<$ty> = ::std::sync::OnceLock::new();
                CELL.get_or_init(|| $make)
            }

            #[wasm_bindgen(wasm_bindgen = $crate::__private::wasm_bindgen)]
            pub fn abi() -> String {
                $crate::ABI.to_string()
            }

            #[wasm_bindgen(wasm_bindgen = $crate::__private::wasm_bindgen)]
            pub fn descriptor() -> String {
                $crate::envelope::descriptor(instance())
            }

            #[wasm_bindgen(wasm_bindgen = $crate::__private::wasm_bindgen)]
            pub fn project(view: &str) -> String {
                $crate::envelope::project(instance(), view)
            }

            #[wasm_bindgen(wasm_bindgen = $crate::__private::wasm_bindgen)]
            pub fn plan(call: &str, view: &str) -> String {
                $crate::envelope::plan(instance(), call, view)
            }

            #[wasm_bindgen(wasm_bindgen = $crate::__private::wasm_bindgen)]
            pub fn validate(delta: &str, view: &str) -> String {
                $crate::envelope::validate(instance(), delta, view)
            }

            #[wasm_bindgen(wasm_bindgen = $crate::__private::wasm_bindgen)]
            pub fn cascade(deleted: &str, view: &str) -> String {
                $crate::envelope::cascade(instance(), deleted, view)
            }

            #[wasm_bindgen(wasm_bindgen = $crate::__private::wasm_bindgen)]
            pub fn digest(view: &str) -> String {
                $crate::envelope::digest(instance(), view)
            }

            #[wasm_bindgen(wasm_bindgen = $crate::__private::wasm_bindgen)]
            pub fn changes(before: &str, after: &str, reader: &str) -> String {
                $crate::envelope::changes(instance(), before, after, reader)
            }

            #[wasm_bindgen(wasm_bindgen = $crate::__private::wasm_bindgen)]
            pub fn legend(view: &str) -> String {
                $crate::envelope::legend(instance(), view)
            }
        }
    };
}
