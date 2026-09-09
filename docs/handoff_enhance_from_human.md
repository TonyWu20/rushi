# Handoff compact mechanism enhancement

We have implemented the rewind, so we need to ship the corresponding facilities. Even if we don't implement the branch summary, we still have to handle multi-branch handoff.

My suggestion:

1. Even without branching, add versioning to `handoff.md`. Preserve more truth. Easier to do a quick proof-read between
   existing versions after a long multi-compacted run to see if the latest compact
   miss something important in earlier round.
2. Study `pi`'s tree + branch summary design and implementation comprehensively.
   Our append-only log can achieve the same effect with masking. We have already
   used masking to achieve the management of compacted region + active region
   without losing the existing fact recorded throughout in the log. We can use
   markers + shadowing to achieve the `tree` or more precisely, a DAG (directional acyclic graph) structure.
   Just need to link the summary/handoff to the divergence point to give them
   the identity + versioning.
