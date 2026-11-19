pub(crate) mod assemble;
pub mod commands;
pub(crate) mod model;
pub(crate) mod read;

pub use commands::diagram_read;
#[cfg(test)]
pub use model::DiagramColumn;
pub use model::{Diagram, DiagramEdge, DiagramTable};
pub(crate) use read::read_diagram;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name::<Diagram>(),
        std::any::type_name_of_val(&diagram_read),
        std::any::type_name_of_val(&read::read_diagram),
        std::any::type_name_of_val(&read::diagram_statements),
    );
    let _ = _linked;
    "diagram"
}

#[cfg(test)]
mod tests;
