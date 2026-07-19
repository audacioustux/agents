#!/bin/sh
# Runs inside the `team` container via the Dokploy Schedule (`docker exec`).
#
# `lean-ctx team sync` alone only does `git fetch --all --prune` — it never
# advances the checked-out tree `team serve` reads. This wraps it with a real
# fetch+reset+clean per workspace, driven by repos.conf (id/url/branch; see
# repos.conf.example), since team.json's workspace schema has no git fields.
set -eu

config=/etc/lean-ctx/team.json
manifest=/etc/lean-ctx/repos.conf
repos_root=/srv/repos

failed=0
while read -r id url branch || [ -n "$id" ]; do
  case "$id" in ''|'#'*) continue ;; esac
  root="$repos_root/$id"
  if [ ! -d "$root/.git" ]; then
    echo "[$id] cloning $url ($branch) -> $root"
    if ! git clone --branch "$branch" --single-branch "$url" "$root"; then
      echo "[$id] clone FAILED, skipping" >&2
      failed=1
    fi
  else
    echo "[$id] fetch+reset to origin/$branch"
    if ! { git -C "$root" fetch origin "$branch" --prune \
        && git -C "$root" checkout -B "$branch" "origin/$branch" \
        && git -C "$root" reset --hard "origin/$branch" \
        && git -C "$root" clean -fdx; }; then
      echo "[$id] fetch+reset FAILED, leaving tree as-is" >&2
      failed=1
    fi
  fi
done <"$manifest"

# `team sync` still does its own fetch + repo-sanity check; run it even if a
# workspace above failed, so healthy workspaces stay current.
lean-ctx team sync --config "$config" || failed=1
exit "$failed"
