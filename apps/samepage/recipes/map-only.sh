#!/usr/bin/env bash
# A second composition: the document and map, with no optional artifacts.
set -euo pipefail
if (( $# != 1 )); then echo 'usage: map-only.sh PROJECT_ROOT' >&2; exit 2; fi
exec "$(dirname "${BASH_SOURCE[0]}")/run.sh" map-only "$1" \
  doc=crates/samepage-doc/web \
  map=crates/samepage-map=apps/samepage/modules/map.js
