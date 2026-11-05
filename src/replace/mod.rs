//! Reemplazo de texto visible: planificación, colores y borrado del original.

pub mod color;
pub mod plan;

pub use color::Rgb;
pub use plan::{
    build, erase_pixels, EraseMode, LinePlan, PageContent, PagePlan, Plan, ReplaceOptions,
    SkipReason, SkippedLine, TextRun,
};
