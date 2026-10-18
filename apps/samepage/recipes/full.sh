#!/usr/bin/env bash
# All twelve independently selected modules, loaded only when needed.
set -euo pipefail
if (( $# != 1 )); then echo 'usage: full.sh PROJECT_ROOT' >&2; exit 2; fi
exec "$(dirname "${BASH_SOURCE[0]}")/run.sh" full "$1" \
  doc=crates/samepage-doc/web \
  map=crates/samepage-map=apps/samepage/modules/map.js \
  diagrams=crates/samepage-diagrams=apps/samepage/modules/diagrams.js \
  review=crates/samepage-review=apps/samepage/modules/review.js \
  architecture=crates/samepage-architecture=apps/samepage/modules/architecture.js \
  board=crates/samepage-board=apps/samepage/modules/board.js \
  sketch=crates/samepage-sketch=apps/samepage/modules/sketch.js \
  trace=crates/samepage-trace=apps/samepage/modules/trace.js \
  panes=crates/samepage-panes=apps/samepage/modules/panes.js \
  sources=crates/samepage-sources=apps/samepage/modules/sources.js \
  lanes=crates/samepage-lanes=apps/samepage/modules/lanes.js \
  cement=crates/samepage-cement=apps/samepage/modules/cement.js \
  explain=crates/samepage-explain=apps/samepage/modules/explain.js
