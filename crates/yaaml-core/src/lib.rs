pub mod config;
pub mod consolidation;
pub mod context;
pub mod eval;
pub mod memory;
pub mod model;
pub mod paths;
pub mod recall;
pub mod status;

pub use config::{Config, ConfigPaths};
pub use consolidation::{find_consolidation_clusters, ClusterMemory, MemoryCluster};
pub use context::{
    context_score, infer_context_from_memory, infer_context_from_path, infer_context_from_text,
    merge_contexts, ContextMetadata,
};
pub use eval::{
    counterfactual_citation_score, memories_created_before, parse_eval_judge_response,
    replay_context_before_turn, EvalJudgeOutcome,
};
pub use memory::{
    derive_project_descriptor, embedded_text_hash, embedding_text, parse_formulation_response,
    MemoryDraft, MemoryError,
};
pub use model::{
    AgentType, EmbeddingRecord, MemoryRecord, MemoryScope, SessionRecord, SourceTurnRef,
    TaskRecord, TaskStatus, TurnRecord, TurnStatus,
};
pub use recall::{
    apply_project_bonus, build_recall_query, cosine_similarity, recall_file_path,
    render_recall_markdown, session_recall_file_path, write_recall_file, RecallCandidate,
    RecallMemory, RecallWrite, VectorHit, VectorIndex,
};
