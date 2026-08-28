//! Apple-platform ecosystems.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const SWIFT: Ecosystem = Ecosystem {
    id: "swift",
    name: "Swift",
    markers: &["Package.swift"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on(".build/")],
};

// `*.xcodeproj` and `*.xcworkspace` are directories rather than files. That is fine: a marker
// is matched by name against a directory listing, not by opening it.
pub(super) const XCODE: Ecosystem = Ecosystem {
    id: "xcode",
    name: "Xcode",
    markers: &["*.xcodeproj", "*.xcworkspace"],
    anchor: Anchor::Sibling,
    artifacts: &[Artifact::on("DerivedData/")],
};
