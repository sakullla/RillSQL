mod assemble;
pub mod commands;
mod model;
mod read;
mod sql;

pub use commands::diagram_read;
pub use model::Diagram;

pub fn module_id() -> &'static str {
    let _linked = (
        std::any::type_name::<Diagram>(),
        std::any::type_name_of_val(&diagram_read),
        std::any::type_name_of_val(&read::read_diagram),
        std::any::type_name_of_val(&sql::diagram_statements),
    );
    let _ = _linked;
    "diagram"
}

#[cfg(test)]
mod tests;
