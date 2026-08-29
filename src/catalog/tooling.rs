//! First-party developer tooling.
//!
//! These are our own tools, which is why they are here and why both entries are on by default:
//! we know what the directories hold, that nothing in them is authored, and that the tool
//! rebuilds them on its next run. A third-party tool gets the same treatment only on the same
//! evidence.

use super::{Anchor, Artifact, Ecosystem};

// NOTE: anchored `Inside` on the `CACHEDIR.TAG` alef writes into every cache it creates, rather
// than on a climb to `alef.toml`. The climb was never soundly bounded: `.alef/` is resolved
// against the *invoking* directory rather than against the config that governs it, so the
// distance is a property of the consumer's repository layout and their working directory, and
// no ancestor bound is correct for it. `Inside` removes the question instead of bounding it.
//
// The tag is written by a helper every cache-creating site routes through, with a test on
// alef's side asserting the root is tagged and not only its children — so this is a guarantee
// rather than an emergent property. A `.alef/` written by an alef too old to tag is left alone;
// that is the safe direction, and one run of a current alef restores it.
//
// `.alef-cache/` is deliberately absent. It looks like alef's and it is not: alef never creates
// it, and the one found in the wild held a zig global cache in zig's own shard layout. See
// 0.3.2, which removed it.
pub(super) const ALEF: Ecosystem = Ecosystem {
    id: "alef",
    name: "alef",
    markers: &["CACHEDIR.TAG"],
    anchor: Anchor::Inside,
    artifacts: &[Artifact::on(".alef/")],
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
