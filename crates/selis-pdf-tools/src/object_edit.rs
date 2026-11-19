//! Tool-layer re-exports for object-level image/vector editing
//! (SL-5.TEXTEDIT.05/06).
//!
//! The algorithms live in [`selis_pdf_edit::objects`]; this module only exposes
//! them as library APIs of the tools crate. There is deliberately **no CLI
//! subcommand and no wasm/web operation** for them yet: selecting an object by
//! clicking on a rendered page needs a hit-test/overlay UI and a tools-protocol
//! request type that do not exist in this slice. The web/UI half is deferred —
//! the engine APIs below are the foundation it will call, and the phase plan
//! says so explicitly.

pub use selis_pdf_edit::objects::{
    delete_object, extract_image, list_image_objects, list_vector_objects, move_object,
    replace_image, resize_object, ImageObject, ObjectEditReport, ObjectError, ObjectId,
    VectorObject,
};
