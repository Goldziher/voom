//! Python.

use super::{Anchor, Artifact, Ecosystem};

pub(super) const PYTHON: Ecosystem = Ecosystem {
    id: "python",
    name: "Python",
    markers: &["pyproject.toml", "setup.py", "setup.cfg"],
    anchor: Anchor::Sibling,
    artifacts: &[
        // `__pycache__/` sits inside package directories rather than beside the manifest, so
        // it is the one Python artifact that climbs. Ancestor(1) is what ADR 0003 specifies;
        // it proves a package one level below the project root and deliberately declines to
        // prove one nested deeper, which is a recall loss rather than a safety one.
        Artifact::on("__pycache__/").at(Anchor::Ancestor(1)),
        Artifact::on(".pytest_cache/"),
        Artifact::on(".mypy_cache/"),
        Artifact::on(".ruff_cache/"),
        Artifact::on(".tox/"),
        Artifact::on("*.egg-info/"),
        Artifact::on("htmlcov/"),
        Artifact::on(".coverage"),
        Artifact::on("build/"),
        Artifact::on("dist/"),
        Artifact::off(
            ".venv/",
            "Removing a virtualenv costs a full reinstall rather than a rebuild.",
        ),
    ],
};
