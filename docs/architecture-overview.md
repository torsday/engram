# Architecture overview

This is the end-user view of how engram works. For the full implementer-facing
design (ADRs, data flow diagrams, schema details), see
[`docs/design/README.md`](design/README.md).

## The four note types

Engram organises your vault around four primary note types, each with a distinct
lifecycle:

```
Fleeting  ──scribe──►  Literature  ──curator──►  Evergreen
   │                                                  │
   │ (raw capture)      (one per source)     (atomic concept)
   │
   └──curator──► Archive   (preserved, inert)
```

| Type           | Created by                 | Lifespan  | Agent role                          |
| -------------- | -------------------------- | --------- | ----------------------------------- |
| **Fleeting**   | You (capture)              | Days      | Scribe cleans; Gardener prunes      |
| **Literature** | Scribe (from fleeting)     | Months    | Curator distills                    |
| **Evergreen**  | Curator (from clusters)    | Years     | Linker connects; Gardener maintains |
| **Archive**    | Curator (corpus digestion) | Permanent | Read-only                           |

MOC (Map of Content) and Journal notes exist too but aren't part of the main
pipeline — MOCs are maintained by Cartographer, journals are left untouched.

## The diff-review workflow

Every agent write goes through the diff-review queue:

```
Agent run
   │
   ▼
Confidence ≥ threshold?
   │ yes                    │ no
   ▼                        ▼
Unstaged diff        Proposal card
in vault git         (you review)
   │                        │
   ▼                        ▼
Diff-review queue ◄─────────┘
   │
   ▼
You: Approve / Reject / Edit
   │
   ▼
git commit (if approved)
```

Agents never run `git add` or `git commit` directly. Every approved change
becomes a commit authored by you. The git history is your audit log.

## Council deliberation

For high-stakes decisions (evergreen synthesis, heretical challenges), engram
convenes a Research Council — a structured multi-agent deliberation:

```
Synthesizer proposes
        │
        ▼
Steelman (strengthens argument)
        │
        ▼
Devil's Advocate (challenges)
        │
        ▼
Inquirer (generates questions)
        │
        ▼
Final proposal → diff-review queue
```

The council produces a `Deliberation` note (stored in `.engram/`) that records
the full reasoning chain. You see the conclusion in the diff-review queue; the
deliberation note gives you the "why" if you want to audit it.

## When agents act versus propose

Agents follow a **confidence-gated autonomy** model (ADR 0004):

| Confidence score | Agent behaviour                                  |
| ---------------- | ------------------------------------------------ |
| ≥ 0.90           | Write directly to vault (still as unstaged diff) |
| 0.70 – 0.89      | Write as unstaged diff + highlight in queue      |
| < 0.70           | Propose only — no write, just a proposal card    |

During the **bootstrap period** (first 30 days), the threshold is raised to 0.95
and every action is highlighted with extra context so you can calibrate your
trust in the agents.

## Privacy routing

Notes under your configured privacy zones (see [`first-run.md`](first-run.md))
are never sent to cloud LLM providers. They are processed locally via Ollama.

The Witness agent monitors all other agent runs for privacy-boundary violations —
any call that would send private-zone content to a cloud provider is intercepted
and blocked before the API call is made.
