pub const INVESTIGATOR: &str = include_str!("../prompts/investigator.md");
pub const VALIDATOR: &str = include_str!("../prompts/validator.md");
pub const RECHECK: &str = include_str!("../prompts/recheck.md");
pub const LEAD_PLAN: &str = include_str!("../prompts/lead_plan.md");
pub const LEAD_SYNTHESIZE: &str = include_str!("../prompts/lead_synthesize.md");
pub const WORKER: &str = include_str!("../prompts/worker.md");
pub const SCOUT_FOCUS: &str = include_str!("../prompts/scout_focus.md");

/// Hash of every prompt text: part of the review identity so edited
/// prompts never reuse a stored result.
pub fn prompt_version() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in [
        INVESTIGATOR,
        VALIDATOR,
        RECHECK,
        LEAD_PLAN,
        LEAD_SYNTHESIZE,
        WORKER,
        SCOUT_FOCUS,
    ] {
        h.update(p.as_bytes());
        h.update([0]);
    }
    for p in crate::checklists::all_texts() {
        h.update(p.as_bytes());
        h.update([0]);
    }
    hex::encode(&h.finalize()[..8])
}
