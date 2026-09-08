/-
RewindSpec — formal specification of the fork-recursion semantics of
`rushi_common::rewind::active_ranges`
(docs/rewind-fork-design.md section 3 and 5, "the active path").

The Lean kernel re-checks every invariant theorem and concrete example.
A clean build with zero unproven claims is the guarantee. See
docs/lean-driven-development.md section 8.

Layout
  * `RewindRef` / `eff` / `lastRewindAtOrBefore` — the event data and
    the chain re-anchoring, mirroring `rushi_common::rewind`.
  * `activeRanges` — an exact mirror of
    `rushi_common::rewind::active_ranges` (the bottom-up range-list
    recursion; a fuel argument keeps the Lean definition total).
  * `walkChain` / `activeSeqs` — the independent reference semantics:
    the active context of a prefix as an ascending list of seqs,
    computed by a single top-down walk of the rewind chain.
  * The invariant theorems (P0 chain-termination, P1 fork-mask, P2
    nested-fork, P3 branch-reentry, plus the shape invariants) bridge
    the two encodings and pin down the active-path recursion.

The parameter formerly named `end` is spelled `pos` throughout: `end`
is a Lean reserved word, so it cannot bind a parameter. `eff` is a
top-level function (not a structure field), so it is always written in
application form `eff r`, never as the invalid projection `r.eff`.
Core-lean only: no `Mathlib` import; `by_cases`, `omega`, and
core tactics (`rw`, `simp`, `cases`) do the case work.
-/

namespace RewindSpec

/-!
## Event data and chain re-anchoring
-/

/-- One parsed `rewind` event, at its 1-based log seq
    (mirrors `rushi_common::rewind::RewindRef`). The parse invariant is
    `target < seq`: a rewind always points at an earlier event. -/
structure RewindRef where
  seq : Nat
  target : Nat
  before : Bool

/-- The effective context boundary: `target` in `on` mode,
    `target - 1` in `before` mode (mirrors `RewindRef::eff`;
    `Nat.sub` saturates at 0, matching `saturating_sub`). -/
def eff (r : RewindRef) : Nat :=
  if r.before then r.target - 1 else r.target

/-- The last rewind in slice order whose seq is at most `pos`
    (mirrors `rewinds.iter().rfind(|r| r.seq <= pos)`). -/
def lastRewindAtOrBefore (pos : Nat) (ws : List RewindRef) : Option RewindRef :=
  match ws with
  | [] => none
  | r :: ws' =>
      match lastRewindAtOrBefore pos ws' with
      | some r' => some r'
      | none => if r.seq ≤ pos then some r else none

/-!
## The active-path recursion (the mirror of `active_ranges`)
-/

/-- The active path of the log prefix ending at seq `pos`, as disjoint
    ascending inclusive `(lo, hi)` seq ranges.

    The definition mirrors `rushi_common::rewind::active_ranges` step
    by step: no rewind at or before `pos` gives `[1..pos]`; otherwise,
    with `r` the last rewind at or before `pos`, the path is
    `active(eff r) ∪ [r.seq+1..pos]`. The fuel argument keeps the Lean
    definition total: one fuel unit is consumed per re-anchor, and
    `activeRanges` supplies `pos + 1` units, which
    `activeRangesFuelIndependent` shows never runs out for valid input
    (every rewind satisfies `target < seq`, the parse invariant). -/
def activeRangesAux (pos : Nat) (ws : List RewindRef) (fuel : List Unit) : List (Nat × Nat) :=
  if pos = 0 then []
  else
    match lastRewindAtOrBefore pos ws with
    | none => [(1, pos)]
    | some r =>
        match fuel with
        | [] => [(1, pos)]
        | _ :: fuel' =>
            if pos > r.seq
            then activeRangesAux (eff r) ws fuel' ++ [(r.seq + 1, pos)]
            else activeRangesAux (eff r) ws fuel'

def activeRanges (pos : Nat) (ws : List RewindRef) : List (Nat × Nat) :=
  activeRangesAux pos ws (List.replicate (pos + 1) ())

/-- Whether log seq `s` is inside one of the ranges, as a Bool
    (mirrors `rushi_common::rewind::seq_in_ranges`). -/
def seqInRanges (s : Nat) (rs : List (Nat × Nat)) : Bool :=
  rs.any (fun pr => if pr.1 ≤ s then (if s ≤ pr.2 then true else false) else false)

/-- Whether log seq `s` is inside one of the ranges, as a `Prop`
    (the form the theorems use). -/
def inRanges (s : Nat) (rs : List (Nat × Nat)) : Prop :=
  ∃ pr ∈ rs, pr.1 ≤ s ∧ s ≤ pr.2

/-!
## The independent reference semantics
-/

/-- One top-down chain step: from `pos`, record the span
    `(r.seq+1, pos]` re-opened by the last marker `r` at or before
    `pos`, then continue at the marker's effective target. Spans are
    in walk order: the first span is the highest (newest) re-opened
    one; the last entry is the base prefix `[1..b]`. -/
def walkChain (pos : Nat) (ws : List RewindRef) (fuel : List Unit) : List (Nat × Nat) :=
  if pos = 0 then []
  else
    match lastRewindAtOrBefore pos ws with
    | none => [(1, pos)]
    | some r =>
        match fuel with
        | [] => [(1, pos)]
        | _ :: fuel' => (r.seq + 1, pos) :: walkChain (eff r) ws fuel'

def walk (pos : Nat) (ws : List RewindRef) : List (Nat × Nat) :=
  walkChain pos ws (List.replicate (pos + 1) ())

/-- The seqs of a span `[lo, hi]` in ascending order; the empty list
    for a degenerate span (`lo > hi`), which a walk only produces for
    a marker that lands at or after the walk point. -/
def spanSeqs (lo hi : Nat) : List Nat :=
  if lo > hi then [] else List.range (hi + 1) |>.filter (fun s => if lo ≤ s then true else false)

/-- Flatten the spans of a walk into the active seqs in ascending
    order: the walk's spans are processed from last (lowest) to first
    (highest), each contributing its seqs. -/
def flattenSpans (ls : List (Nat × Nat)) : List Nat :=
  match ls with
  | [] => []
  | span :: rest =>
      flattenSpans rest ++ spanSeqs span.1 span.2

/-- The active context of the prefix ending at `pos`, as the
    ascending list of active 1-based seqs — the independent reference
    semantics. Structurally a different construction from
    `activeRanges` (which builds the range list bottom-up out of the
    recursion); `activeRangesSeqsAgree` proves the two encodings
    agree on every seq. -/
def activeSeqs (pos : Nat) (ws : List RewindRef) : List Nat :=
  flattenSpans (walk pos ws)

/-- The two concrete constructors the examples use. -/
def rOn (s t : Nat) : RewindRef := { seq := s, target := t, before := false }
def rBefore (s t : Nat) : RewindRef := { seq := s, target := t, before := true }

/-!
## Helper lemmas
-/

/-- A rewind's effective target is strictly before its own marker,
    under the parse invariant `target < seq`. -/
theorem effLtSeq (r : RewindRef) (h : r.target < r.seq) : eff r < r.seq := by
  simp only [eff]
  by_cases hb : r.before = true
  · simp [hb]
    by_cases h0 : r.target = 0
    · rw [h0]
      simp only [Nat.zero_sub]
      rw [h0] at h
      exact h
    · have hpos : 0 < r.target := by omega
      omega
  · simp [hb]
    exact h

/-- The marker the chain re-anchors at is at or before the walk point
    (the recursion's decreasing step). -/
theorem lastRewindSeqLe (pos : Nat) (ws : List RewindRef) (r : RewindRef) :
    lastRewindAtOrBefore pos ws = some r → r.seq ≤ pos := by
  induction ws with
  | nil =>
    intro h
    simp only [lastRewindAtOrBefore] at h
    cases h
  | cons w ws' ih =>
    intro h
    simp only [lastRewindAtOrBefore] at h
    cases hopt : lastRewindAtOrBefore pos ws' with
    | some w' =>
      rw [hopt] at h
      exact ih (hopt.trans h)
    | none =>
      simp [hopt] at h
      by_cases hw : w.seq ≤ pos
      · have hso : some w = some r := by simpa [hw] using h
        have hwr : w = r := Option.some_inj.mp hso
        rw [← hwr]
        exact hw
      · simp [hw] at h

/-- The marker the chain re-anchors at is one of the given rewinds. -/
theorem lastRewindIn (pos : Nat) (ws : List RewindRef) (r : RewindRef) :
    lastRewindAtOrBefore pos ws = some r → r ∈ ws := by
  induction ws with
  | nil =>
    intro h
    simp only [lastRewindAtOrBefore] at h
    cases h
  | cons w ws' ih =>
    intro h
    simp only [lastRewindAtOrBefore] at h
    cases hopt : lastRewindAtOrBefore pos ws' with
    | some w' =>
      rw [hopt] at h
      exact List.mem_cons.mpr (Or.inr (ih (hopt.trans h)))
    | none =>
      rw [hopt] at h
      by_cases hw : w.seq ≤ pos
      · have hso : some w = some r := by simpa [hw] using h
        have hwr : w = r := Option.some_inj.mp hso
        subst hwr
        exact List.mem_cons.mpr (Or.inl rfl)
      · simp [hw] at h

/-- The seqs of a span `[lo, hi]` are exactly the seqs `s` with
    `lo ≤ s ≤ hi`. -/
theorem spanSeqsMem (s lo hi : Nat) : s ∈ spanSeqs lo hi ↔ lo ≤ s ∧ s ≤ hi := by
  by_cases hdeg : lo > hi
  · -- lo > hi: spanSeqs = []
    simp only [spanSeqs, hdeg]
    constructor
    · intro h
      cases h
    · intro hq
      exfalso
      omega
  · -- lo ≤ hi: spanSeqs = the range [0, hi] filtered to s ≥ lo
    have hle : lo ≤ hi := by omega
    simp [spanSeqs, hle, List.mem_filter, List.mem_range, Nat.lt_succ_iff]
    rw [and_comm]

/-- `inRanges` over a single range: `s` is in `[lo..hi]` iff
    `lo ≤ s ≤ hi`. -/
theorem singleRange (s lo hi : Nat) : inRanges s [(lo, hi)] ↔ lo ≤ s ∧ s ≤ hi := by
  simp only [inRanges, List.mem_cons]
  constructor
  · intro h
    rcases h with ⟨pr, hmem, hconj⟩
    cases hmem with
    | inl heq =>
      subst heq
      exact hconj
    | inr hnil =>
      cases hnil
  · intro h
    exact ⟨(lo, hi), ⟨Or.inl rfl, h⟩⟩

/-- `inRanges` over an appended singleton: split the membership into
    the tail list and the appended range. -/
theorem inRangesAppendSingleton (s : Nat) (A : List (Nat × Nat)) (lo hi : Nat) :
    inRanges s (A ++ [(lo, hi)]) ↔ inRanges s A ∨ inRanges s [(lo, hi)] := by
  simp only [inRanges, List.mem_append]
  constructor
  · intro h
    rcases h with ⟨pr, hmem, hconj⟩
    cases hmem with
    | inl hA =>
      exact Or.inl ⟨pr, hA, hconj⟩
    | inr hsp =>
      exact Or.inr ⟨pr, hsp, hconj⟩
  · intro h
    cases h with
    | inl h =>
      rcases h with ⟨pr, hmem, hconj⟩
      exact ⟨pr, Or.inl hmem, hconj⟩
    | inr h =>
      rcases h with ⟨pr, hmem, hconj⟩
      exact ⟨pr, Or.inr hmem, hconj⟩

/-- Every span recorded by the walk at `pos` has its hi at most `pos`
    (for valid input the walk never re-opens above the walk point). -/
theorem walkSpansBelow (ws : List RewindRef) (hvalid : ∀ r ∈ ws, r.target < r.seq) :
    ∀ (pos : Nat) (fuel : List Unit), ∀ sp ∈ walkChain pos ws fuel, sp.2 ≤ pos := by
  intro pos fuel
  revert pos
  induction fuel with
  | nil =>
    intro pos sp hsp
    cases pos with
    | zero =>
      have hw0 : walkChain 0 ws [] = [] := by
        rw [walkChain.eq_def]
        simp
      rw [hw0] at hsp
      cases hsp
    | succ c =>
      have h : walkChain (c + 1) ws [] = [(1, c + 1)] := by
        cases hlast : lastRewindAtOrBefore (c + 1) ws with
        | none =>
          rw [walkChain.eq_def, hlast]
          simp
        | some r =>
          rw [walkChain.eq_def, hlast]
          simp
      rw [h] at hsp
      simp only [List.mem_cons] at hsp
      cases hsp with
      | inl heq =>
        subst heq
        omega
      | inr hnil =>
        cases hnil
  | cons _ f' ih =>
    intro pos sp hsp
    cases pos with
    | zero =>
      have hw0 : walkChain 0 ws (() :: f') = [] := by
        rw [walkChain.eq_def]
        simp
      rw [hw0] at hsp
      cases hsp
    | succ c =>
      cases hopt : lastRewindAtOrBefore (c + 1) ws with
      | none =>
        have h : walkChain (c + 1) ws (() :: f') = [(1, c + 1)] := by
          rw [walkChain.eq_def, hopt]
          simp
        rw [h] at hsp
        simp only [List.mem_cons] at hsp
        cases hsp with
        | inl heq =>
          subst heq
          omega
        | inr hnil =>
          cases hnil
      | some r =>
        have hmem : r ∈ ws := lastRewindIn (c + 1) ws r hopt
        have hv := hvalid r hmem
        have hseq : r.seq ≤ c + 1 := lastRewindSeqLe (c + 1) ws r hopt
        have heff : eff r < c + 1 := Nat.lt_of_lt_of_le (effLtSeq r hv) hseq
        have h : walkChain (c + 1) ws (() :: f') =
            (r.seq + 1, c + 1) :: walkChain (eff r) ws f' := by
          rw [walkChain.eq_def, hopt]
          by_cases h0 : c + 1 = 0
          · exfalso
            omega
          · simp
        rw [h] at hsp
        simp only [List.mem_cons] at hsp
        cases hsp with
        | inl heq =>
          subst heq
          omega
        | inr hrest =>
          have hrest' : sp.2 ≤ eff r := ih (eff r) sp hrest
          have hle : eff r ≤ c + 1 := Nat.le_of_lt heff
          exact Nat.le_trans hrest' hle

/-- If every span of `L` ends strictly below `s`, then `s` is in no
    span, hence `s ∉ flattenSpans L`. -/
theorem flattenSpansBelow (L : List (Nat × Nat)) (s : Nat)
    (h : ∀ sp ∈ L, sp.2 < s) : s ∉ flattenSpans L := by
  induction L with
  | nil =>
    intro hs
    simp only [flattenSpans] at hs
    cases hs
  | cons sp L' ih =>
    intro hs
    simp only [flattenSpans] at hs
    rw [List.mem_append] at hs
    cases hs with
    | inl hsL' =>
      have hbelow : ∀ sp' ∈ L', sp'.2 < s := fun sp' hmem =>
        h sp' (List.mem_cons.mpr (Or.inr hmem))
      have hno : s ∉ flattenSpans L' := ih hbelow
      exact hno hsL'
    | inr hsSpan =>
      have hspan : s ∉ spanSeqs sp.1 sp.2 := by
        intro hsq
        rw [spanSeqsMem] at hsq
        have hbound : sp.2 < s := h sp (List.mem_cons.mpr (Or.inl rfl))
        exfalso
        omega
      exact hspan hsSpan

/-!
## P0 — chain-termination (fuel independence)

The recursion terminates: every re-anchor strictly decreases the walk
point (`eff r < r.seq ≤ pos`), and the fuel of `pos + 1` units is
never exhausted — the result is independent of the fuel whenever the
fuel exceeds the walk point.
-/

/-- For valid input the range-list result does not depend on the fuel,
    as long as the fuel exceeds the walk point: the chain bottoms out
    within `pos` steps. -/
theorem auxFuelIndependent (ws : List RewindRef) (hvalid : ∀ r ∈ ws, r.target < r.seq) :
    ∀ (pos : Nat) (f1 f2 : List Unit),
      f1.length > pos → f2.length > pos → activeRangesAux pos ws f1 = activeRangesAux pos ws f2 := by
  intro pos
  refine Nat.strongRecOn pos fun c ih => ?_
  intro f1 f2 hf1 hf2
  cases c with
  | zero =>
    simp [activeRangesAux]
  | succ c =>
    cases hlast : lastRewindAtOrBefore (c + 1) ws with
    | none =>
      have h1 : activeRangesAux (c + 1) ws f1 = [(1, c + 1)] := by
        rw [activeRangesAux.eq_def, hlast]
        simp
      have h2 : activeRangesAux (c + 1) ws f2 = [(1, c + 1)] := by
        rw [activeRangesAux.eq_def, hlast]
        simp
      rw [h1, h2]
    | some r =>
      have hmem : r ∈ ws := lastRewindIn (c + 1) ws r hlast
      have hv := hvalid r hmem
      have hseq : r.seq ≤ c + 1 := lastRewindSeqLe (c + 1) ws r hlast
      have heff : eff r < c + 1 := Nat.lt_of_lt_of_le (effLtSeq r hv) hseq
      have htail1 : (List.tail f1).length > eff r := by
        have hlen : (List.tail f1).length = f1.length - 1 := List.length_tail
        rw [hlen]
        omega
      have htail2 : (List.tail f2).length > eff r := by
        have hlen : (List.tail f2).length = f2.length - 1 := List.length_tail
        rw [hlen]
        omega
      have hIH : activeRangesAux (eff r) ws (List.tail f1) = activeRangesAux (eff r) ws (List.tail f2) :=
        ih (eff r) heff (List.tail f1) (List.tail f2) htail1 htail2
      have hlhs : activeRangesAux (c + 1) ws f1 =
          (if c + 1 > r.seq
           then activeRangesAux (eff r) ws (List.tail f1) ++ [(r.seq + 1, c + 1)]
           else activeRangesAux (eff r) ws (List.tail f1)) := by
        rw [activeRangesAux.eq_def, hlast]
        by_cases h0 : c + 1 = 0
        · exfalso
          omega
        · cases f1 with
          | nil => exfalso; simp at hf1
          | cons _ _ => simp [List.tail]
      have hrs : activeRangesAux (c + 1) ws f2 =
          (if c + 1 > r.seq
           then activeRangesAux (eff r) ws (List.tail f2) ++ [(r.seq + 1, c + 1)]
           else activeRangesAux (eff r) ws (List.tail f2)) := by
        rw [activeRangesAux.eq_def, hlast]
        by_cases h0 : c + 1 = 0
        · exfalso
          omega
        · cases f2 with
          | nil => exfalso; simp at hf2
          | cons _ _ => simp [List.tail]
      rw [hlhs, hrs]
      by_cases hpush : c + 1 > r.seq
      · simp [hpush, hIH]
      · simp [hpush, hIH]

theorem activeRangesFuelIndependent (pos : Nat) (ws : List RewindRef)
    (hvalid : ∀ r ∈ ws, r.target < r.seq) (f : List Unit) (hf : f.length > pos) :
    activeRanges pos ws = activeRangesAux pos ws f := by
  rw [activeRanges]
  apply (auxFuelIndependent ws hvalid pos (List.replicate (pos + 1) ()) f)
  · rw [List.length_replicate]
    omega
  · exact hf

/-!
## The bridge: the range encoding equals the reference semantics

`activeSeqs` is the independent reference (a single top-down walk of the
rewind chain); `activeRanges` is the bottom-up range-list recursion the
Rust mirrors. The bridge says the two encodings agree on every seq —
this is the correctness invariant that pins the active-path recursion.
-/

theorem bridgeAux (s pos : Nat) (ws : List RewindRef) (f : List Unit) :
    inRanges s (activeRangesAux pos ws f) ↔ s ∈ flattenSpans (walkChain pos ws f) := by
  revert pos
  induction f with
  | nil =>
    intro pos
    cases pos with
    | zero =>
      have hr : activeRangesAux 0 ws [] = [] := by
        rw [activeRangesAux.eq_def]
        simp
      have hw : walkChain 0 ws [] = [] := by
        rw [walkChain.eq_def]
        simp
      rw [hr, hw]
      constructor
      · intro h
        rcases h with ⟨_, hmem, _⟩
        cases hmem
      · intro h
        cases h
    | succ c =>
      cases hlast : lastRewindAtOrBefore (c + 1) ws with
      | none =>
        have hr : activeRangesAux (c + 1) ws [] = [(1, c + 1)] := by
          rw [activeRangesAux.eq_def, hlast]
          simp
        have hw : walkChain (c + 1) ws [] = [(1, c + 1)] := by
          rw [walkChain.eq_def, hlast]
          simp
        rw [hr, hw]
        have h1 : inRanges s [(1, c + 1)] ↔ 1 ≤ s ∧ s ≤ c + 1 := singleRange s 1 (c + 1)
        have h2 : s ∈ flattenSpans [(1, c + 1)] ↔ 1 ≤ s ∧ s ≤ c + 1 := by
          simp only [flattenSpans, List.nil_append]
          rw [spanSeqsMem]
        rw [h1, h2]
      | some r =>
        have hr : activeRangesAux (c + 1) ws [] = [(1, c + 1)] := by
          rw [activeRangesAux.eq_def, hlast]
          simp
        have hw : walkChain (c + 1) ws [] = [(1, c + 1)] := by
          rw [walkChain.eq_def, hlast]
          simp
        rw [hr, hw]
        have h1 : inRanges s [(1, c + 1)] ↔ 1 ≤ s ∧ s ≤ c + 1 := singleRange s 1 (c + 1)
        have h2 : s ∈ flattenSpans [(1, c + 1)] ↔ 1 ≤ s ∧ s ≤ c + 1 := by
          simp only [flattenSpans, List.nil_append]
          rw [spanSeqsMem]
        rw [h1, h2]
  | cons _ f' ih =>
    intro pos
    cases pos with
    | zero =>
      have hr : activeRangesAux 0 ws (() :: f') = [] := by
        rw [activeRangesAux.eq_def]
        simp
      have hw : walkChain 0 ws (() :: f') = [] := by
        rw [walkChain.eq_def]
        simp
      rw [hr, hw]
      constructor
      · intro h
        rcases h with ⟨_, hmem, _⟩
        cases hmem
      · intro h
        cases h
    | succ c =>
      cases hlast : lastRewindAtOrBefore (c + 1) ws with
      | none =>
        have hr : activeRangesAux (c + 1) ws (() :: f') = [(1, c + 1)] := by
          rw [activeRangesAux.eq_def, hlast]
          simp
        have hw : walkChain (c + 1) ws (() :: f') = [(1, c + 1)] := by
          rw [walkChain.eq_def, hlast]
          simp
        rw [hr, hw]
        have h1 : inRanges s [(1, c + 1)] ↔ 1 ≤ s ∧ s ≤ c + 1 := singleRange s 1 (c + 1)
        have h2 : s ∈ flattenSpans [(1, c + 1)] ↔ 1 ≤ s ∧ s ≤ c + 1 := by
          simp only [flattenSpans, List.nil_append]
          rw [spanSeqsMem]
        rw [h1, h2]
      | some r =>
        have hr : activeRangesAux (c + 1) ws (() :: f') =
            (if c + 1 > r.seq
             then activeRangesAux (eff r) ws f' ++ [(r.seq + 1, c + 1)]
             else activeRangesAux (eff r) ws f') := by
          rw [activeRangesAux.eq_def, hlast]
          by_cases h0 : c + 1 = 0
          · exfalso
            omega
          · simp
        have hw : walkChain (c + 1) ws (() :: f') =
            (r.seq + 1, c + 1) :: walkChain (eff r) ws f' := by
          rw [walkChain.eq_def, hlast]
          by_cases h0 : c + 1 = 0
          · exfalso
            omega
          · simp
        rw [hr, hw]
        have hmem : s ∈ flattenSpans (walkChain (eff r) ws f') ++ spanSeqs (r.seq + 1) (c + 1) ↔
            s ∈ flattenSpans (walkChain (eff r) ws f') ∨ s ∈ spanSeqs (r.seq + 1) (c + 1) := by
          rw [List.mem_append]
        have hspan : s ∈ spanSeqs (r.seq + 1) (c + 1) ↔ r.seq + 1 ≤ s ∧ s ≤ c + 1 :=
          spanSeqsMem s (r.seq + 1) (c + 1)
        by_cases hpush : c + 1 > r.seq
        · -- c + 1 > r.seq: the range list appends the span
          simp [hpush]
          rw [inRangesAppendSingleton, singleRange]
          simp only [flattenSpans]
          rw [hmem, hspan]
          rw [ih (eff r)]
        · -- not c + 1 > r.seq: the span is degenerate (r.seq + 1 > c + 1)
          simp [hpush]
          have hdeg : spanSeqs (r.seq + 1) (c + 1) = [] := by
            by_cases hd : r.seq + 1 > c + 1
            · simp [spanSeqs, hd]
            · exfalso
              omega
          simp only [flattenSpans]
          rw [hdeg, List.append_nil]
          rw [ih (eff r)]

theorem activeRangesSeqsAgree (s pos : Nat) (ws : List RewindRef) :
    inRanges s (activeRanges pos ws) ↔ s ∈ activeSeqs pos ws := by
  rw [activeRanges, activeSeqs, walk]
  apply bridgeAux s pos ws (List.replicate (pos + 1) ())

/-!
## P1 — fork-mask and P3 — branch re-entry

For a marker `r` that re-anchors at `pos`, the span abandoned by the
fork `(eff r, r.seq]` is masked (no seq in it is active), and the
span after the marker `[r.seq+1, pos]` is active. Together with the
bridge, this is the fork-mask / branch-reentry invariant of
docs/rewind-fork-design.md.
-/

/-- The masked gap of the latest fork: no seq in `(eff r, r.seq]` is
    active in the context of the prefix. -/
theorem forkMasksAbandonedSpan (pos : Nat) (ws : List RewindRef)
    (hvalid : ∀ r ∈ ws, r.target < r.seq) (r : RewindRef)
    (hlast : lastRewindAtOrBefore pos ws = some r) :
    ∀ s, eff r < s → s ≤ r.seq → s ∉ activeSeqs pos ws := by
  intro s hgt hle
  cases pos with
  | zero =>
    have hseq : r.seq = 0 := by
      have h : r.seq ≤ 0 := lastRewindSeqLe 0 ws r hlast
      exact Nat.eq_zero_of_le_zero h
    have hs : s = 0 := by simpa [hseq] using hle
    subst hs
    exfalso
    omega
  | succ c =>
    have hlast' : lastRewindAtOrBefore (c + 1) ws = some r := by simpa using hlast
    intro hs
    simp only [activeSeqs, walk] at hs
    have hwalk : walkChain (c + 1) ws (List.replicate (c + 2) ()) =
        (r.seq + 1, c + 1) :: walkChain (eff r) ws (List.replicate (c + 1) ()) := by
      rw [walkChain.eq_def, hlast']
      by_cases h0 : c + 1 = 0
      · exfalso
        omega
      · simp [List.replicate_succ]
    rw [hwalk] at hs
    simp only [flattenSpans] at hs
    rw [List.mem_append] at hs
    cases hs with
    | inl hsRest =>
      -- s is in no span of the walk below eff r (they all end ≤ eff r < s)
      have hbelow : ∀ sp ∈ walkChain (eff r) ws (List.replicate (c + 1) ()), sp.2 < s := by
        intro sp hmem
        have hsp : sp.2 ≤ eff r :=
          (walkSpansBelow ws hvalid) (eff r) (List.replicate (c + 1) ()) sp hmem
        exact Nat.lt_of_le_of_lt hsp hgt
      have hno : s ∉ flattenSpans (walkChain (eff r) ws (List.replicate (c + 1) ())) :=
        flattenSpansBelow (walkChain (eff r) ws (List.replicate (c + 1) ())) s hbelow
      exact hno hsRest
    | inr hsSpan =>
      -- s ∉ spanSeqs (r.seq + 1) (c + 1): s ≤ r.seq < r.seq + 1
      have hno : s ∉ spanSeqs (r.seq + 1) (c + 1) := by
        intro hsq
        rw [spanSeqsMem] at hsq
        have hlow : ¬ r.seq + 1 ≤ s := by
          intro hl
          omega
        exfalso
        exact hlow hsq.1
      exact hno hsSpan

/-- The re-opened tail after the marker is active: a seq strictly
    after the marker but at or before the walk point is on the active
    path (branch re-entry). -/
theorem forkKeepsTail (pos : Nat) (ws : List RewindRef) (r : RewindRef)
    (hlast : lastRewindAtOrBefore pos ws = some r) :
    ∀ s, r.seq < s → s ≤ pos → s ∈ activeSeqs pos ws := by
  intro s hgt hle
  cases pos with
  | zero =>
    exfalso
    omega
  | succ c =>
    have hlast' : lastRewindAtOrBefore (c + 1) ws = some r := by simpa using hlast
    rw [activeSeqs, walk]
    have hwalk : walkChain (c + 1) ws (List.replicate (c + 2) ()) =
        (r.seq + 1, c + 1) :: walkChain (eff r) ws (List.replicate (c + 1) ()) := by
      rw [walkChain.eq_def, hlast']
      by_cases h0 : c + 1 = 0
      · exfalso
        omega
      · simp [List.replicate_succ]
    rw [hwalk]
    simp only [flattenSpans]
    rw [List.mem_append]
    right
    rw [spanSeqsMem]
    constructor
    · omega
    · omega

/-!
## P2 / P3 — concrete counter-examples of the single-gap rule

Mirror the Rust unit tests in `crates/rushi/src/rewind.rs`. These are
decided by the kernel on closed terms (`decide`), pinning the exact
active range lists the implementation must emit.
-/

/-- P2: after fork A→B, fork B→A', and a rewind inside A', branch B
    is masked (the single-gap rule's counter-example). -/
theorem nestedForksMaskAbandonedBranch :
    activeRanges 10 [rOn 4 3, rOn 7 3, rOn 10 9] = [(1, 3), (8, 9)] := by
  decide

/-- P3: re-entering a forked branch rebuilds its full active path,
    with the sibling branch masked. -/
theorem reenteringRebuildsBranch :
    activeRanges 12 [rOn 4 3, rOn 7 3, rOn 10 6] = [(1, 3), (5, 6), (11, 12)] := by
  decide

/-- The depth-3 chain composes: 13 → 8 → 5 → 3, and a prefix inside
    the chain (10) sees its own branch. -/
theorem deepChainsFollowNestedTargets :
    activeRanges 13 [rOn 4 3, rOn 8 5, rOn 11 8] = [(1, 3), (5, 5), (12, 13)] ∧
    activeRanges 10 [rOn 4 3, rOn 8 5, rOn 11 8] = [(1, 3), (5, 5), (9, 10)] := by
  decide

/-- Rewinds later than the prefix end are not in play: the prefix
    context is frozen at the point it was reached. -/
theorem rewindsAfterThePrefixDoNotApply :
    activeRanges 5 [rOn 4 1, rOn 9 3] = [(1, 1), (5, 5)] ∧
    activeRanges 9 [rOn 4 1, rOn 9 3] = [(1, 3)] := by
  decide

/-- `on` mode includes the target; `before` mode excludes it; a
    `before` target at seq 1 leaves no base prefix. -/
theorem onModeIncludesTarget :
    activeRanges 6 [rOn 4 3] = [(1, 3), (5, 6)] := by
  decide

theorem beforeModeExcludesTarget :
    activeRanges 7 [rBefore 6 4] = [(1, 3), (7, 7)] := by
  decide

theorem beforeAtSeqOneCoversNothing :
    activeRanges 4 [rBefore 3 1] = [(4, 4)] := by
  decide

/-- A rewind that lands exactly on its event's own prefix has no
    continuation range: the context is the target path. -/
theorem rewindAtLogEndHasNoContinuation :
    activeRanges 5 [rOn 5 2] = [(1, 2)] := by
  decide

theorem activeRangesEmpty (ws : List RewindRef) : activeRanges 0 ws = [] := by
  simp [activeRanges, activeRangesAux]

theorem noRewindsFullPrefix (pos : Nat) :
    activeRanges pos [] = if pos = 0 then [] else [(1, pos)] := by
  cases pos with
  | zero => simp [activeRanges, activeRangesAux]
  | succ c =>
    simp [activeRanges, activeRangesAux, lastRewindAtOrBefore]

end RewindSpec
