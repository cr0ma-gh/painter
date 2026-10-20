//! Several documents in one session: tabs, background open and save,
//! a reference view of another document beside the canvas, and copying
//! layers and selections between documents.
//!
//! - [`session_store`]: the open documents and switching between them.
//! - [`tab_bar`]: the document tabs, their shortcuts and the close / quit prompts.
//! - [`split_viewport`]: the Reference View editor.
//! - [`layer_transfer`]: layers, folders, 3D layers and selections into another tab.

pub(crate) mod layer_transfer;
pub(crate) mod session_store;
pub(crate) mod split_viewport;
pub(crate) mod tab_bar;

pub(crate) use layer_transfer::LayerDrag;
pub(crate) use session_store::DocManager;
