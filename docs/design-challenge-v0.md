# Challenge and claims, v0

Board two, first half, of the ADR-shaped Same Page session. Written 2026-09-03.
The other half (registers, lens, cement to ADR and G8) is not in this document.

## The problem this solves

The atlas is one-to-one with what the agent believes. That is the point, and
it stays. What it lacks is room for the part of an architecture review where a
human picks one spot and asks a dozen questions until everyone in the room
holds the same understanding. Today that discussion happens in the transcript
and scrolls away, and the picture cannot show which parts are agreed and which
are still being argued.

The resolution is that uncertainty and disagreement are part of the agent's
understanding, so they belong on the page. A node under challenge looks
different from a node that is agreed. That is still verbatim visual proof.

## Vocabulary

- **Challenge.** The human asks the agent to lay out what it understands about
  one node. A challenge is not an object. It is the existing `?` mark on the
  node, with a text the browser writes for the human: `challenge: lay out what
  you understand about this, one claim at a time, with the basis for each`.
- **Claim.** One statement the agent makes about a node, with a stated basis.
  A new CRDT kind, `claim`. Claims are what the human accepts or rejects.
- **Basis.** How the agent knows. One of `verified`, `inferred`, `assumed`,
  `unknown`. `verified` requires `path` and `lines` and the host checks that
  the range exists, exactly like a source-backed card. `inferred` may carry a
  path. `assumed` and `unknown` carry no source, and if one is given it is
  refused so the label cannot lie.
- **Verdict.** What the human said about a claim. `open` until the human
  acts, then `accepted` or `rejected`. Only the human writes a verdict, the
  same rule as marks. The agent may `withdraw` a claim it no longer stands
  behind; a withdrawn claim keeps its history and stops counting.
- **Under challenge.** A node with at least one claim whose verdict is `open`
  or `rejected`. This is derived, never stored.
- **Settled.** A node that has claims and none of them are open or rejected.

## The claim object

Stored as a CRDT object of kind `claim` with these properties:

| prop | type | who writes | notes |
|------|------|-----------|-------|
| `about` | node id | agent at create | must exist; removing the node removes its claims |
| `text` | string, MAX_TEXT | agent | the statement itself |
| `basis` | one of BASES | agent | `verified` needs `path` and `lines` |
| `path` | string | agent | repository-relative |
| `lines` | string | agent | `"12-48"` or `"12"` |
| `verdict` | one of VERDICTS | human only | `open`, `accepted`, `rejected` |
| `withdrawn` | bool | agent only | set once, never cleared |
| `created_by` | author | system | |
| `touched_by` | author | system | per-key LWW, same as nodes |

Constants: `BASES = ["verified", "inferred", "assumed", "unknown"]`,
`VERDICTS = ["open", "accepted", "rejected"]`, `MAX_CLAIMS_PER_NODE = 24`.
Twenty-four because a review that needs more claims on one node is telling
you the node is two nodes; the refusal message says so.

Claims count toward `MAX_OBJECTS`.

## Rules the core enforces

1. A claim must be about a node that exists. A claim about a shape, mark, or
   constraint is refused: claims are about understanding, and only nodes carry
   understanding.
2. `verified` without `path` and `lines` is refused. `assumed` or `unknown`
   with a path is refused.
3. Only the human can write `verdict`. Only the agent can create, revise, or
   withdraw a claim. A revision of a claim whose verdict is `accepted` resets
   the verdict to `open`, because the human accepted different words.
4. A node cannot be set to `status = agreed` while it is under challenge. The
   refusal names the open claims.
5. Marks may target claims. `?` on a claim means the human does not follow
   that claim; `!` means they think it is wrong. The agent answers with
   `atlas_answer` as today, or revises the claim.
6. Removing a node removes its claims and any marks on those claims.
7. Projection heals a claim whose node no longer exists by dropping it, the
   same way `parent` is healed to the top level.

## Read-back

`atlas_read` gains one section, after MARKS and before PROBLEMS:

```
CLAIMS (what the agent says it understands, and what the human said back)
- "Persistence" UNDER CHALLENGE: 3 claims, 1 open, 1 rejected, 1 accepted
    [c12] verified crates/ag-ui-surface/src/activity.rs:187-240 "The journal writes every action to disk before acknowledging it" ACCEPTED by human
    [c13] inferred "Eviction happens at 800 events" REJECTED by human
        ? [m4] "where is 800 set?" UNANSWERED
    [c14] assumed "Nothing reads the journal but the trace panel" open
- "Camera" SETTLED: 2 claims, both accepted
```

A node with no claims prints nothing here. The summary line uses the words
UNDER CHALLENGE and SETTLED verbatim so a model can grep its own read-back.

PROBLEMS gains three sentences:

- `node "X" is marked agreed but has N open claim(s)` (cannot happen through
  the core, but an imported document can carry it)
- `claim c13 on "X" is verified but its source no longer resolves`
- `node "X" has been under challenge for N reads without a new claim or
  verdict` is NOT included; time is not document state.

`CHANGED SINCE YOUR LAST READ` gains:

- `claim c12 on "Persistence" was accepted by human`
- `claim c13 on "Persistence" was rejected by human`
- `claim c13 on "Persistence" changed basis inferred -> verified`
- `claim c14 on "Persistence" was withdrawn by agent`
- `"Persistence" is now SETTLED` / `"Persistence" is now UNDER CHALLENGE`

## Tools

- `atlas_claim` (agent only). Create or revise. `{ about, text, basis, path?,
  lines?, id? }`. With `id` it revises. Returns the claim id and the node's
  current summary line. The host verifies `path` and `lines` for `verified`
  and `inferred` with the same `verify_source` used for cards.
- `atlas_withdraw_claim` (agent only). `{ id }`.
- `atlas_answer` accepts a mark id whose target is a claim.
- `atlas_read` unchanged in shape; the section above is added.

Browser (human only), through `AtlasDoc`:

- `challenge(node_id)` writes the `?` mark with the fixed challenge text.
- `claim_verdict(id, verdict)` writes `accepted` or `rejected`.
- `mark(target, glyph, text)` already exists and now accepts claim ids.

## Browser

- Each node card and each container frame shows its claims below its marks:
  one row per claim with a basis chip (`verified` shows the source, clickable
  the same way a card source is), the text, and for open claims two buttons,
  Accept and Reject. Accepted claims show a check, rejected a cross, withdrawn
  are struck through and dimmed.
- A node under challenge gets `data-challenge="open"` and a badge with the
  open plus rejected count. A settled node gets `data-challenge="settled"`.
  Styles: an outline in the human colour for open, a thin outline in the
  agreed green for settled. Frames get the same attribute so a challenged
  container reads at the 50,000 foot level too.
- The node context actions gain `Challenge`. It writes the mark and reports
  `Challenge opened on "X". The agent will answer with claims you can accept
  or reject.`
- No innerHTML. No rAF polling. The claim list re-renders from the snapshot
  the same way marks do.

## Agent guidance (prompt.md)

One numbered item: when a `?` mark carries the challenge text, do not answer
it in prose. Answer with one `atlas_claim` per thing you believe, stating the
basis truthfully, then `atlas_answer` the mark with one line saying how many
claims you wrote. When a claim is rejected, either revise it with a better
basis or withdraw it, and say which in the transcript. Never write a verdict.

## Not in scope this board

- Altitude lens (collapse to depth). Container collapse exists and is enough
  for the talk.
- Cement to ADR and G8. Settled is the precondition, not the act.
- Per-object registers and the open-differences panel across the whole
  document. This board shows differences per node only.
- Claims about shapes, segments, or agent-ink variables.
- Any judge model. Verdicts are the human's, always.

## Fixture

`examples/same-page-atlas/fixtures/challenge/persistence.json`: a three node
hierarchy (Atlas host, Persistence, Activity journal) with three claims on
Persistence in the three states shown in the read-back above, plus one `?`
mark on the rejected claim. The content-pinned test asserts the exact CLAIMS
section text.
