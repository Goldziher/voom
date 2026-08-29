//! First-party developer tooling.
//!
//! These are our own tools, which is why they are here and why both entries are on by default:
//! we know what the directories hold, that nothing in them is authored, and that the tool
//! rebuilds them on its next run. A third-party tool gets the same treatment only on the same
//! evidence.

use super::{Anchor, Artifact, Ecosystem};

// NOTE: `alef.toml` sits at the repository root while `.alef/` is written wherever alef is
// invoked — beside the root config, beside a crate two levels down, beside a language package
// three or six levels down. Measured across one workspace: 69 directories at climb distances
// 0 (7), 2 (58), 3 (3) and 6 (1). `Ancestor(6)` is that measurement plus nothing, and the
// catalog's own 8-level ceiling is the ceiling on how wrong it can be.
pub(super) const ALEF: Ecosystem = Ecosystem {
    id: "alef",
    name: "alef",
    markers: &["alef.toml"],
    anchor: Anchor::Ancestor(6),
    artifacts: &[Artifact::on(".alef/"), Artifact::on(".alef-cache/")],
};

// NOTE: anchored `Inside` rather than on a sibling `basemind.toml`, because the config file is
// optional and usually absent — measured at 3 of 13 real directories, and the largest of them
// was in the other 10. What *is* always present is `agent-id`, which basemind writes when it
// creates the index. A file of that name inside a directory of this name is the tool's own
// declaration, and it is the only thing that reliably distinguishes the index from a directory
// somebody else called `.basemind`.
//
// Off by default, unlike alef. The proof is just as good; the cost of being right is not. An
// index is rebuilt by re-reading and re-embedding the whole repository rather than by
// recompiling it, which is minutes rather than seconds, and `agent-id` and the telemetry log
// beside it are identity and history rather than derived output — the new index is not the old
// one. That is the shape `Artifact::off` exists for.
pub(super) const BASEMIND: Ecosystem = Ecosystem {
    id: "basemind",
    name: "basemind",
    markers: &["agent-id"],
    anchor: Anchor::Inside,
    artifacts: &[Artifact::off(
        ".basemind/",
        "Rebuilding the index means re-reading and re-embedding the whole repository, and the \
         agent identity and telemetry beside it do not come back.",
    )],
};
