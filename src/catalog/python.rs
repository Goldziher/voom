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
        Artifact::off(
            ".tox/",
            "Each `.tox/<env>/` is a virtualenv: removing it costs a full reinstall rather than \
             a rebuild, exactly as for `.venv/`.",
        ),
        Artifact::on("*.egg-info/"),
        Artifact::on("htmlcov/"),
        Artifact::on(".coverage"),
        Artifact::off(
            "build/",
            "`build/` beside a Python manifest is as often a hand-written build-script \
             directory as output, the same reason node's is off.",
        ),
        Artifact::on("dist/"),
        Artifact::off(
            ".venv/",
            "Removing a virtualenv costs a full reinstall rather than a rebuild.",
        ),
    ],
};
