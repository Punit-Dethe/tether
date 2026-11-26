#!/usr/bin/env bash
# Build a made-up fork to try tether on, or to take the README screenshots.
#
#   scripts/demo-fork.sh <empty-dir>        then: cd <dir>/lumen-studio && tether ui
#
# "lumen" is an imaginary notes app; "lumen-studio" is a product built from a
# downloaded copy of it. Upstream has moved on by three releases since:
# translations, CI bumps, a security fix, and changes that collide with the
# fork's own work. Everything here (names, PRs, people) is invented.
set -euo pipefail

TETHER=${TETHER:-tether}
root=${1:?usage: demo-fork.sh <empty-dir>}
mkdir -p "$root"
root=$(cd "$root" && pwd)
up="$root/lumen"
fork="$root/lumen-studio"

# A fixed clock, one step per commit, so every run builds the same history.
clock=$(( $(date +%s) - 45 * 86400 ))
tick() { clock=$((clock + ${1:-100000})); }

repo() {
  git init -q -b main "$1"
  git -C "$1" config user.name "${2:-Lumen Maintainers}"
  git -C "$1" config user.email "maintainers@lumen.invalid"
  git -C "$1" config commit.gpgsign false
  git -C "$1" config core.autocrlf false
}
put() { mkdir -p "$(dirname "$1/$2")"; printf '%s\n' "$3" >"$1/$2"; }
commit() { # repo author message
  tick
  git -C "$1" add -A
  GIT_AUTHOR_NAME="$2" GIT_AUTHOR_EMAIL="dev@lumen.invalid" \
    GIT_AUTHOR_DATE="@$clock +0000" GIT_COMMITTER_DATE="@$clock +0000" \
    git -C "$1" commit -q --allow-empty -m "$3"
}
tag() { git -C "$up" tag -a "$1" -m "$1"; }

# --- upstream: lumen 1.3.0 ---------------------------------------------------
repo "$up"
put "$up" README.md "# Lumen
Fast, local notes."
put "$up" src/editor.rs "pub fn open(path: &str) -> Editor { Editor::new(path) }
pub fn autosave_secs() -> u32 { 30 }"
put "$up" src/search.rs "pub fn rank(q: &str) -> Vec<Hit> { fuzzy(q) }
pub const LIMIT: usize = 50;"
put "$up" src/sync.rs "pub fn push() { http::post(\"/sync\") }"
put "$up" src/export/html.rs "pub fn render(note: &Note) -> String { note.body.clone() }"
put "$up" src/settings.rs "pub struct Settings { pub theme: Theme }"
put "$up" locales/en.json '{ "open": "Open", "save": "Save" }'
put "$up" docs/guide.md "# Guide"
put "$up" .github/workflows/ci.yml "uses: actions/checkout@v4"
commit "$up" "Mara Lind" "Release 1.3.0"
tag v1.3.0
base=$(git -C "$up" rev-parse HEAD)

# --- the fork: a downloaded copy, then its own work ---------------------------
repo "$fork" "Studio Team"
cp -r "$up/README.md" "$up/src" "$up/locales" "$up/docs" "$up/.github" "$fork/"
commit "$fork" "Studio Team" "Start Lumen Studio from Lumen 1.3.0"
git -C "$fork" remote add upstream "$up"
git -C "$fork" fetch -q upstream
"$TETHER" -C "$fork" init --sync-point "$base" >/dev/null
cat >>"$fork/.tether/config.toml" <<'EOF'

# Studio keeps its own translations: upstream's never need merging by hand.
[[owned]]
path = "locales/"
EOF
sed -i -e 's|^\[upstream\]$|[upstream]\nname = "Lumen"|' \
  -e 's|^\[map\]$|[map]\nscope = ["src/"]|' "$fork/.tether/config.toml"
commit "$fork" "Studio Team" "Track Lumen with tether"

put "$fork" README.md "# Lumen Studio
Notes for teams, built on Lumen."
put "$fork" src/editor.rs "pub fn open(path: &str) -> Editor { Editor::shared(path) } // tether: Studio opens notes in shared sessions
pub fn autosave_secs() -> u32 { 5 } // tether: teams need near-live saves"
put "$fork" src/search.rs "pub fn rank(q: &str) -> Vec<Hit> { semantic(q) } // tether: Studio ranks with embeddings
pub const LIMIT: usize = 50;"
put "$fork" src/studio/teams.rs "pub struct Team { pub members: Vec<User> }"
put "$fork" locales/en.json '{ "open": "Open", "save": "Save", "share": "Share" }'
commit "$fork" "Studio Team" "Shared editing, semantic search and teams"

# --- upstream moves on: 1.4.0, 1.5.0, 1.5.1 and an unreleased tail ------------
u() { commit "$up" "$1" "$2"; }
put "$up" docs/guide.md "# Guide
## Shortcuts";                                  u "Theo Okafor" "Document keyboard shortcuts (#381)"
put "$up" locales/fr.json '{ "open": "Ouvrir" }';  u "Camille Roux" "Add French translation (#383)"
put "$up" .github/workflows/ci.yml "uses: actions/checkout@v5"; u "dependabot[bot]" "Bump actions/checkout from 4 to 5 (#384)"
put "$up" src/sync.rs "pub fn push() { http::post(\"/sync\").retry(3) }"; u "Mara Lind" "Retry sync on flaky networks (#386)"
put "$up" locales/de.json '{ "open": "Öffnen" }';  u "Jonas Weber" "Add German translation (#388)"
put "$up" src/settings.rs "pub struct Settings { pub theme: Theme, pub font: Font }"; u "Priya Raman" "Let people pick the editor font (#390)"
tag v1.4.0
put "$up" src/export/html.rs "pub fn render(note: &Note) -> String { escape(&note.body) }"
u "Mara Lind" "Escape HTML in exported notes (#394)

Notes with <script> tags ran in the exported page. GHSA-7f3q-lumn-x2v9."
put "$up" locales/ja.json '{ "open": "開く" }';    u "Ren Takahashi" "Add Japanese translation (#395)"
put "$up" src/search.rs "pub fn rank(q: &str) -> Vec<Hit> { fuzzy(q).dedup() }
pub const LIMIT: usize = 50;";                   u "Theo Okafor" "Drop duplicate search hits (#397)"
put "$up" docs/guide.md "# Guide
## Shortcuts
## Export";                                      u "Theo Okafor" "Explain export formats (#398)"
put "$up" .github/workflows/ci.yml "uses: actions/checkout@v5
cache: true";                                    u "Priya Raman" "Cache dependencies in CI (#399)"
put "$up" locales/en.json '{ "open": "Open", "save": "Save", "export": "Export" }'; u "Camille Roux" "Add the Export string (#401)"
tag v1.5.0
put "$up" src/editor.rs "pub fn open(path: &str) -> Editor { Editor::new(path) }
pub fn autosave_secs() -> u32 { 15 }";            u "Mara Lind" "Autosave every 15 seconds (#404)"
put "$up" locales/es.json '{ "open": "Abrir" }';   u "Lucía Ortega" "Add Spanish translation (#405)"
put "$up" .github/workflows/release.yml "on: tag"; u "Priya Raman" "Publish builds from tags (#406)"
tag v1.5.1
put "$up" src/sync.rs "pub fn push() { http::post(\"/v2/sync\").retry(3) }"; u "Mara Lind" "Move sync to the v2 API (#409)"
put "$up" src/editor.rs "pub fn open(path: &str) -> Editor { Editor::with_history(path) }
pub fn autosave_secs() -> u32 { 15 }";            u "Theo Okafor" "Keep undo history across restarts (#411)"
put "$up" src/auth.rs "pub fn refresh(t: &Token) -> Token { t.verify().renew() }"
u "Priya Raman" "Verify tokens before refreshing them (#413)

Fixes CVE-2026-31337."
put "$up" locales/it.json '{ "open": "Apri" }';    u "Giulia Conti" "Add Italian translation (#414)"

# --- what the fork's people already did --------------------------------------
t() { "$TETHER" -C "$fork" "$@" >/dev/null || { "$TETHER" -C "$fork" "$@"; exit 1; }; }
"$TETHER" -C "$fork" sync --dry-run >/dev/null || true # fetches upstream and its release tags
t folder Translations "#383" "#388" "#395" "#405" "#414"
t folder CI "#384" "#399" "#406"
t decide "#381" --note "Our guide is separate; worth borrowing the shortcut table."
t decide "#390" ignored --note "Studio has its own font picker."
commit "$fork" "Studio Team" "Record upstream decisions"

echo "Demo fork ready: $fork"
echo "Try:  cd \"$fork\" && $TETHER ui"
