/-
Formal backstop for the `rushi setup` tool-set resolver.

This module mirrors the pure function `resolve_tools` in
`bin/rushi/src/setup.rs` and checks it with the Lean 4 kernel.
The mapping follows docs/lean-driven-development.md.

The model is faithful to the classification logic. The Rust
function keeps kernel and local as BTreeSet<String> and checks
membership with contains (a bool). The model uses List String
for each, and the same if/else chain. Duplicates in those lists
do not affect the result. The final sort step in Rust is not
modeled: sorting only reorders each output list and never
changes its content.

Every invariant below is stated over content (membership and
multiplicity), not order.
-/



namespace RushiSpec

/-- The `[tools] enabled` list from `rushi.toml`. -/
abbrev EnabledTools := List String

/-- Mirror of `MaterializePlan` in `bin/rushi/src/setup.rs`. -/
structure Plan where
  /- Tools copied from the kernel install. -/
  copyFromKernel : EnabledTools
  /- Tools already present locally. Left untouched. -/
  alreadyLocal : EnabledTools
  /- Tools in neither the kernel nor local. -/
  missing : EnabledTools

/-- Where a tool name comes from. Mirrors the if/else chain in
    resolve_tools: local masks kernel (P7). -/
inductive Origin
| fromLocal
| fromKernel
| fromMissing

def classify (n : String) (kernel localTools : List String) : Origin :=
  if n ∈ localTools then Origin.fromLocal
  else if n ∈ kernel then Origin.fromKernel
  else Origin.fromMissing

def stepOrigin (n : String) (origin : Origin) (p : Plan) : Plan :=
  match origin with
  | Origin.fromLocal  => { p with alreadyLocal := p.alreadyLocal ++ [n] }
  | Origin.fromKernel => { p with copyFromKernel := p.copyFromKernel ++ [n] }
  | Origin.fromMissing => { p with missing := p.missing ++ [n] }

def step (n : String) (kernel localTools : List String) (p : Plan) : Plan :=
  stepOrigin n (classify n kernel localTools) p

/-- Mirror of `resolve_tools` in `bin/rushi/src/setup.rs`. -/
def resolve (enabled : EnabledTools) (kernel localTools : List String) : Plan :=
  match enabled with
  | []      => { copyFromKernel := [], alreadyLocal := [], missing := [] }
  | n :: ns => step n kernel localTools (resolve ns kernel localTools)

/-!
## Invariants

All invariants are stated as multiplicity invariants over the
three output lists. Order is irrelevant; `List.count` from
Mathlib tracks how many copies of a name appear.
-/

/-! ### P-partition: no name is lost or duplicated -/

/-- Appending a classified name to a plan increases the total
    bucket count by exactly `List.count n [m]`, regardless of
    which bucket receives it. -/
theorem step_sum (n m : String) (kernel localTools : List String) (p : Plan) :
    List.count n (step m kernel localTools p).copyFromKernel +
    List.count n (step m kernel localTools p).alreadyLocal +
    List.count n (step m kernel localTools p).missing =
    List.count n p.copyFromKernel + List.count n p.alreadyLocal +
    List.count n p.missing +
    List.count n [m] := by
  unfold step
  cases (classify m kernel localTools) with
  | fromLocal =>
    unfold stepOrigin
    dsimp only
    simp [List.count_append, List.count_cons, List.count_nil,
      Nat.add_assoc, Nat.add_comm, Nat.add_left_comm]
  | fromKernel =>
    unfold stepOrigin
    dsimp only
    simp [List.count_append, List.count_cons, List.count_nil,
      Nat.add_assoc, Nat.add_comm, Nat.add_left_comm]
  | fromMissing =>
    unfold stepOrigin
    dsimp only
    simp [List.count_append, List.count_cons, List.count_nil,
      Nat.add_assoc]

/-- For every name n, the three buckets together hold exactly
    `List.count n enabled` copies of n. -/
theorem total_count (enabled : EnabledTools) (kernel localTools : List String) (n : String) :
    List.count n (resolve enabled kernel localTools).copyFromKernel +
    List.count n (resolve enabled kernel localTools).alreadyLocal +
    List.count n (resolve enabled kernel localTools).missing =
    List.count n enabled := by
  induction enabled with
  | nil =>
    simp [resolve, List.count_nil, Nat.add_zero]
  | cons m ns ih =>
    unfold resolve
    have hstep := step_sum n m kernel localTools (resolve ns kernel localTools)
    rw [hstep, ih]
    by_cases hn : n = m
    · simp [hn, List.count_nil]
    · simp [List.count_cons, List.count_nil]

/-! ### P7: a local tool masks the kernel -/

/-- If n is in localTools, then no copy of n appears in
    copyFromKernel. -/
theorem local_masks_kernel (enabled : EnabledTools) (kernel localTools : List String)
    (n : String) (hn : n ∈ localTools) :
    List.count n (resolve enabled kernel localTools).copyFromKernel = 0 := by
  induction enabled with
  | nil =>
    simp [resolve, List.count_nil]
  | cons m ns ih =>
    simp only [resolve, step, stepOrigin, classify]
    by_cases hmL : m ∈ localTools
    · -- m is local: not added to copyFromKernel
      simp [hmL]
      exact ih
    · -- m is not in localTools
      by_cases hmK : m ∈ kernel
      · -- m is in kernel: added to copyFromKernel
        have hmneq : m ≠ n := by
          intro h
          rw [← h] at hn
          exact hmL hn
        have hcnt : List.count n [m] = 0 := by
          simp [hmneq, List.count_nil]
        simp [hmL, hmK, List.count_append, hcnt, Nat.add_zero]
        exact ih
      · -- m is in neither: not added to copyFromKernel
        simp [hmL, hmK]
        exact ih

/-! ### P10: a missing tool is in neither kernel nor local -/

/-- If n is in kernel or in localTools, then no copy of n appears
    in the missing bucket. -/
theorem not_missing_when_available (enabled : EnabledTools) (kernel localTools : List String)
    (n : String) (hn : n ∈ kernel ∨ n ∈ localTools) :
    List.count n (resolve enabled kernel localTools).missing = 0 := by
  induction enabled with
  | nil =>
    simp [resolve, List.count_nil]
  | cons m ns ih =>
    simp only [resolve, step, stepOrigin, classify]
    by_cases hmL : m ∈ localTools
    · -- m is local: not added to missing
      simp [hmL]
      exact ih
    · -- m is not in localTools
      by_cases hmK : m ∈ kernel
      · -- m is in kernel: not added to missing
        simp [hmL, hmK]
        exact ih
      · -- m is in neither: added to missing
        have hmneq : m ≠ n := by
          intro h
          rw [← h] at hn
          cases hn with
          | inl hk => exact hmK hk
          | inr hl => exact hmL hl
        have hcnt : List.count n [m] = 0 := by
          simp [hmneq, List.count_nil]
        simp [hmL, hmK, List.count_append, hcnt, Nat.add_zero]
        exact ih

/-!
## Concrete examples (mirror the Rust unit tests in
`bin/rushi/src/setup.rs`)
-/

/-- Mirrors `resolve_tools_all_kernel`: every enabled name is in
    kernel, local is empty, so every name is copied. -/
theorem example_all_kernel :
    List.count "bash"
      (resolve ["read", "write", "bash"]
        ["read", "write", "edit", "list", "bash", "goal"] []).copyFromKernel = 1 := by
  decide

/-- Mirrors `resolve_tools_local_wins`: `read` is local, so it is
    not copied from the kernel. -/
theorem example_local_wins :
    List.count "read"
      (resolve ["read", "custom"]
        ["read", "write"] ["read"]).copyFromKernel = 0 := by
  decide

/-- Mirrors `resolve_tools_missing_reported`: `nonexistent` is in
    neither set, so it is reported as missing. -/
theorem example_missing :
    List.count "nonexistent"
      (resolve ["read", "nonexistent"]
        ["read"] []).missing = 1 := by
  decide

end RushiSpec
