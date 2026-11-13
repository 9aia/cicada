#!/usr/bin/env bash
# cicada release — thin shim over tarsi-serve (the org release plane,
# project ADR-0005; repo config: ./tarsi.toml). One call runs
# plan → changelog → bump → tag → push; publish targets are off
# (ADR-0004: crates.io is tamias-only, GitHub Releases unused).
# Operator-run on main — `tarsi-serve release` is dry-run by
# default; --yes is the explicit opt-in.
#
# crates carry per-crate versions (no workspace-inherited version),
# so units bump independently; the `cicada` pack version tracks the
# `cicada` unit (crates/cicada-cli, package `cicada`).
#
# Preserved repo step: the tamias pack manifest + README install
# ref are synced to the `cicada` unit's new version *before* the
# release commit lands. The pack publish itself stays manual
# (`cargo xtask pack` stages the driver payload, then
# `tamias publish libraries/cicada`).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
# worktree-local env (gitignored); the service env file is canonical
[ -f ./.env ] && { set -a; source ./.env; set +a; }

# Learn the `cicada` unit's post-bump version (empty = don't sync).
new="$(tarsi-serve plan --dir . | python3 -c '
import json,sys
d=json.load(sys.stdin)
if not d.get("ok"): sys.exit(0)
for u in d["result"]["plan"]["units"]:
    if u.get("releases") and u["spec"]["name"] == "cicada":
        print(u["new_version"]); break
')"
old="$(grep -m1 '^version' crates/cicada-cli/Cargo.toml | cut -d'"' -f2)"
if [ -n "$new" ] && [ "$new" != "$old" ]; then
    sed -i "s/^version: \"${old}\"/version: \"${new}\"/" libraries/cicada/tamias.yaml
    sed -i "s|cicada@${old}|cicada@${new}|g" README.md
    git add libraries/cicada/tamias.yaml README.md
    git commit -qm "chore(release): sync cicada@${new} version refs ahead of tarsi bump"
fi

tarsi-serve release --dir . --yes

echo "released: cicada ${new:-$old} (tag cicada-v${new:-$old}, per-unit tags for the rest)"
echo "next: cargo xtask pack && tamias publish libraries/cicada  # ship 9aia:cicada@${new:-$old} to the org registry"
