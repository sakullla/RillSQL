mod editor_proto;
mod format_viewer;
mod foundation;
mod grid_proto;
pub mod kit;

pub use editor_proto::{ObjectHandler, ObjectLink, SqlEditor};
#[allow(unused_imports)] // 后续任务的公开入口，本任务内尚无调用方。
pub use format_viewer::{
    apply_block, commit_draft, copy_node_value, detect_format, editing_locked, editor_language, effective_format, format_key, format_sql,
    format_xml, hex_dump, hex_dump_source, minify_json, parse_json, pretty_json, scalar_hint, scalar_hint_text, truncation_banner, validate_json,
    value_at_path, visible_json_nodes, yaml_view, ApplyBlock, CommitResult, DataFormat, DetectInput, EditMode, FormatViewer, FormatViewerEvent,
    JsonDiagnostic, JsonScalarKind, JsonTreeNode, ScalarHint, ScalarHintKind, TruncationBanner, TruncationState, ViewerSpec, YamlView,
    YAML_NOT_STRICT_KEY,
};
pub use foundation::Foundation;
pub use grid_proto::{GridFixture, GridPrototype};
