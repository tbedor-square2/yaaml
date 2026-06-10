pub mod claude;
pub mod codex;
pub mod discovery;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnHydration {
    pub display_text: Option<String>,
    pub cwd: Option<String>,
    pub context: Option<yaaml_core::ContextMetadata>,
}
