/-
TuiViewportSpec — Formal specification for the viewport-based scrollback
design.

Source requirement
  docs/tui_feature_requests_from_human.md:
    "When in browse mode, updates from model response should not flush
     the screen to the latest position of the conversation."

Model
  * The full session log is an ordered list of lines (oldest first).
  * The TUI holds a contiguous window of that log in memory.
  * A viewport of height `h` slides over the window, positioned by
    `scroll` lines from the tail (scroll = 0 means follow the tail).
  * New lines append at the tail; the user can extend the window
    backward (load older lines) or evict lines from the front.

Invariants (proven below)
  P1    no-flush  while pinned (scroll > 0), appending lines does not
                  move the viewport
  P2    follow    when following the tail (scroll = 0), the viewport
                  tracks the newest lines
  P2b   tail-new  when following the tail and the appended batch is at
                  least viewport-sized, the viewport is entirely new
                  lines (the user sees the newest content, not stale
                  lines shifted by the append)
  P3    evict     evicting lines that are above the viewport is safe;
                  the visible content is unchanged
  P4    reach     extending the window backward by the missing `lo`
                  lines reaches the start of the log (no hard cap)
  P4b   chunked   repeated chunked backward extension reaches the log
                  start; after `n` chunks of `c` lines, `max 0 (lo -
                  n * c)` lines remain
  P5    bounded   while following the tail, the loaded window can be
                  evicted down to `h + buffer` lines; the window stays
                  bounded and tail-following is preserved

All theorems are proven; the Lean kernel re-checks every step.
A clean build with zero unproven claims is the guarantee.
See docs/lean-driven-development.md §8.
-/

namespace TuiViewportSpec

/-! ## Model -/

/-- A rendered transcript line (abstract). -/
abbrev Line := Nat

/-- Viewport state over a contiguous window of transcript lines. -/
structure View where
  /-- The loaded lines, oldest first. -/
  lines  : List Line
  /-- Index (in the full log) of the first loaded line. 0 = log start. -/
  lo     : Nat
  /-- Lines scrolled up from the tail. 0 = follow the tail. -/
  scroll : Nat
  /-- Viewport height in lines. -/
  h      : Nat

/-- 0-indexed start of the viewport within `lines` (saturating:
    truncated subtraction, as in the Rust `usize::saturating_sub`). -/
def startIdx (v : View) : Nat :=
  v.lines.length - (v.scroll + v.h)

/-- The lines currently visible in the viewport. -/
def visible (v : View) : List Line :=
  v.lines.drop (startIdx v) |>.take v.h

/-- Append `new` lines at the tail (new events arrived).
    If pinned (scroll > 0), compensate the scroll offset so the
    viewport content is unchanged.  If following the tail (scroll = 0),
    keep scroll at 0 so the view advances to the new tail. -/
def onAppend (v : View) (new : List Line) : View :=
  { lines  := v.lines ++ new
  , lo     := v.lo
  , scroll := if v.scroll = 0 then 0 else v.scroll + new.length
  , h      := v.h }

/-- Extend the window backward by prepending `older` lines.
    Decreases `lo` by `older.length` (saturating). -/
def extendBack (v : View) (older : List Line) : View :=
  { lines  := older ++ v.lines
  , lo     := v.lo - older.length
  , scroll := v.scroll
  , h      := v.h }

/-- Evict `k` lines from the front of the loaded window. -/
def evictFront (v : View) (k : Nat) : View :=
  { lines  := v.lines.drop k
  , lo     := v.lo + k
  , scroll := v.scroll
  , h      := v.h }

/-- One chunk of backward extension loads up to `c` older lines
    (saturating at the log start). -/
def chunkLo (lo c : Nat) : Nat :=
  lo - min c lo

/-- Distance to the log start after `n` chunks of `c` lines. -/
def chunksLeft (lo c n : Nat) : Nat :=
  match n with
  | 0 => lo
  | m + 1 => chunkLo (chunksLeft lo c m) c

/-! ## Helper lemmas -/

/-- Adding the same amount to both sides of a Nat subtraction
    leaves the result unchanged. -/
theorem nat_sub_shift (a b n : Nat) : (a + n) - (b + n) = a - b := by
  by_cases H : a < b
  · -- a < b: both sides are 0
    have h1 : a + n < b + n := Nat.add_lt_add_right H n
    rw [Nat.sub_eq_zero_of_le (Nat.le_of_lt h1)]
    rw [Nat.sub_eq_zero_of_le (Nat.le_of_lt H)]
  · -- a ≥ b
    have hge : b ≤ a := by
      -- For Nat: ¬(a < b) → b ≤ a
      have hab : a ≤ b ∨ b ≤ a := Nat.le_total a b
      cases hab with
      | inl ha =>
        have heq : a = b := by
          by_cases Hne : a = b
          · exact Hne
          · have hlt : a < b := Nat.lt_of_le_of_ne ha Hne
            exact False.elim (H hlt)
        rw [heq]
        exact Nat.le_refl b
      | inr hb => exact hb
    -- b ≤ a: both sides are actual subtractions
    have h1 : b + n ≤ a + n := Nat.add_le_add_right hge n
    have h_left : ((a + n) - (b + n)) + (b + n) = a + n :=
      Nat.sub_add_cancel h1
    have h_right : (a - b) + b = a := Nat.sub_add_cancel hge
    have h_right2 : (a - b) + (b + n) = a + n := by
      rw [← Nat.add_assoc, h_right]
    have h_eq : ((a + n) - (b + n)) + (b + n) = (a - b) + (b + n) := by
      rw [h_left, ← h_right2]
    exact Nat.add_right_cancel h_eq

/-- `a - (a - b) = b` when `b ≤ a`. -/
theorem nat_sub_sub_self (a b : Nat) (h : b ≤ a) : a - (a - b) = b := by
  have hsum : b + (a - b) = a := by
    rw [Nat.add_comm]
    exact Nat.sub_add_cancel h
  calc
    a - (a - b) = b + (a - b) - (a - b) :=
      congrArg (fun x : Nat => x - (a - b)) hsum.symm
    _ = b := Nat.add_sub_cancel b (a - b)

/-- Evicting `k` lines from the front of a window of length `L`
    shifts the front index by `k`: when `k + T ≤ L`,
    `k + ((L - k) - T) = L - T`. -/
theorem nat_evict_shift (L k T : Nat) (h : k + T ≤ L) :
    k + ((L - k) - T) = L - T := by
  rw [Nat.sub_sub]
  -- Goal: k + (L - (k + T)) = L - T
  have hT : T ≤ L := Nat.le_trans (Nat.le_add_left T k) h
  have hsum : (k + (L - (k + T))) + T = (L - T) + T := by
    rw [Nat.add_comm k (L - (k + T)), Nat.add_assoc]
    rw [Nat.sub_add_cancel h, Nat.sub_add_cancel hT]
  exact Nat.add_right_cancel hsum

/-! ## Invariants -/

/-- **P1: no-flush (pinned view).** When the user is pinned
    (`scroll > 0`) and the viewport fits within the loaded lines
    (`h ≤ lines.length`), appending new lines and compensating the
    scroll offset leaves the visible content unchanged. -/
theorem P1_no_flush (v : View) (new : List Line)
    (hp : 0 < v.scroll) (hfit : v.h ≤ v.lines.length) :
    visible (onAppend v new) = visible v := by
  dsimp only [visible, onAppend, startIdx]
  -- Goal: ((v.lines ++ new).drop S').take h = (v.lines.drop S).take h, where
  --   S' = (L + n) - ((if v.scroll = 0 then 0 else s + n) + h)
  --   S  = L - (s + h)
  -- Step 1: the start index does not move: S' = S (using s > 0)
  have hnz : v.scroll ≠ 0 := by
    intro hz
    rw [hz] at hp
    exact False.elim (Nat.lt_irrefl 0 hp)
  have hS : (v.lines.length + new.length) -
            ((if v.scroll = 0 then 0 else v.scroll + new.length) + v.h) =
           v.lines.length - (v.scroll + v.h) := by
    have havg : (v.scroll + new.length) + v.h = (v.scroll + v.h) + new.length := by
      rw [Nat.add_assoc]
      rw [show new.length + v.h = v.h + new.length from Nat.add_comm new.length v.h]
      rw [← Nat.add_assoc v.scroll v.h new.length]
    rw [if_neg hnz, havg, nat_sub_shift v.lines.length (v.scroll + v.h) new.length]
  rw [List.length_append, hS]
  -- Step 2: S ≤ L, so dropping S from (lines ++ new) hands off to `new`:
  have hSle : v.lines.length - (v.scroll + v.h) ≤ v.lines.length := Nat.sub_le _ _
  have hSsub : v.lines.length - (v.scroll + v.h) - v.lines.length = 0 :=
    Nat.sub_eq_zero_of_le hSle
  rw [List.drop_append, hSsub, List.drop_zero]
  -- Step 3: take h of (A ++ new) keeps the old head when h ≤ A.length
  have hAlen : (v.lines.drop (v.lines.length - (v.scroll + v.h))).length =
           v.lines.length - (v.lines.length - (v.scroll + v.h)) := by
    rw [List.length_drop]
  have hzero : v.h - (v.lines.drop (v.lines.length - (v.scroll + v.h))).length = 0 := by
    rw [hAlen]
    by_cases Hsh : v.scroll + v.h ≤ v.lines.length
    · -- S is exact: A.length = s + h, and s > 0, so h < s + h
      have hA : v.lines.length - (v.lines.length - (v.scroll + v.h)) =
               v.scroll + v.h :=
        nat_sub_sub_self v.lines.length (v.scroll + v.h) Hsh
      rw [hA]
      exact Nat.sub_eq_zero_of_le (Nat.le_add_left v.h v.scroll)
    · -- S = 0: A.length = L, and h ≤ L (hfit)
      have hS0 : v.lines.length - (v.scroll + v.h) = 0 :=
        Nat.sub_eq_zero_of_le (Nat.le_of_lt (Nat.lt_of_not_le Hsh))
      rw [hS0, Nat.sub_zero]
      exact Nat.sub_eq_zero_of_le hfit
  rw [List.take_append, hzero, List.take_zero, List.append_nil]

/-- **P2: follow-tail.** When the view is following the tail
    (`scroll = 0`), appending new lines keeps the view at the new
    tail: the viewport starts at the position `min h total` lines
    before the new tail. -/
theorem P2_follow_tail (v : View) (new : List Line) (ht : v.scroll = 0) :
    (onAppend v new).scroll = 0 ∧
    startIdx (onAppend v new) =
      (v.lines.length + new.length) - v.h := by
  dsimp only [onAppend, startIdx]
  constructor
  · simp [ht]
  · simp [ht]

/-- **P2b: tail-new.** While following the tail, if the appended
    batch is at least viewport-sized, the entire viewport consists of
    new lines (the last `h` of the batch). -/
theorem P2b_new_tail_visible (v : View) (new : List Line)
    (ht : v.scroll = 0) (hn : new.length ≥ v.h) :
    visible (onAppend v new) = new.drop (new.length - v.h) := by
  dsimp only [visible, onAppend, startIdx]
  -- scroll' = 0, so the start is (L + n) - h
  have hstart : (v.lines.length + new.length) -
                ((if v.scroll = 0 then 0 else v.scroll + new.length) + v.h) =
               (v.lines.length + new.length) - v.h := by
    simp [ht, Nat.zero_add]
  rw [List.length_append, hstart]
  -- The drop offset reaches into `new`: L + n - h = L + (n - h) ≥ L
  have hshift : v.lines.length + new.length - v.h =
               v.lines.length + (new.length - v.h) :=
    Nat.add_sub_assoc hn v.lines.length
  rw [hshift]
  -- Dropping L + (n - h) ≥ L lines from a list of L lines gives []:
  have hdrop : v.lines.drop (v.lines.length + (new.length - v.h)) = [] := by
    have hlen : (v.lines.drop (v.lines.length + (new.length - v.h))).length = 0 := by
      rw [List.length_drop]
      exact Nat.sub_eq_zero_of_le (Nat.le_add_right v.lines.length (new.length - v.h))
    rw [List.length_eq_zero_iff.mp hlen]
  -- The handoff into `new` starts at (n - h):
  have hX : (v.lines.length + (new.length - v.h)) - v.lines.length = new.length - v.h :=
    Nat.add_sub_cancel_left v.lines.length (new.length - v.h)
  rw [List.drop_append, hdrop, hX, List.nil_append]
  -- `new.drop (n - h)` holds exactly h lines, so taking h is the identity
  rw [List.take_drop, Nat.sub_add_cancel hn, List.take_length]

/-- **P3: eviction safety.** Evicting `k` lines from the front,
    where `k ≤ startIdx`, leaves the visible content unchanged.
    The evicted lines are all above the viewport. -/
theorem P3_evict_safe (v : View) (k : Nat)
    (hk : k ≤ startIdx v) :
    visible (evictFront v k) = visible v := by
  dsimp only [visible, evictFront, startIdx] at *
  -- Normal form: S' = (L - k) - (s + h), via List.length_drop
  rw [List.length_drop]
  -- LHS: ((v.lines.drop k).drop S').take h, where S' = (L - k) - (s + h)
  -- RHS: (v.lines.drop S).take h,       where S  = L - (s + h)
  by_cases Hsh : v.scroll + v.h ≤ v.lines.length
  · -- S is exact; k ≤ S gives k + T ≤ L, so k + S' = S
    have hkT : k + (v.scroll + v.h) ≤ v.lines.length := by
      have hadd : k + (v.scroll + v.h) ≤
                (v.lines.length - (v.scroll + v.h)) + (v.scroll + v.h) :=
        Nat.add_le_add_right hk (v.scroll + v.h)
      have hmid : (v.lines.length - (v.scroll + v.h)) + (v.scroll + v.h) =
                v.lines.length := Nat.sub_add_cancel Hsh
      rw [← hmid]
      exact hadd
    have hshift : k + ((v.lines.length - k) - (v.scroll + v.h)) =
                 v.lines.length - (v.scroll + v.h) :=
      nat_evict_shift v.lines.length k (v.scroll + v.h) hkT
    rw [List.drop_drop, hshift]
  · -- S = 0; k ≤ 0, so k = 0: both sides are identical
    have hS0 : v.lines.length - (v.scroll + v.h) = 0 :=
      Nat.sub_eq_zero_of_le (Nat.le_of_lt (Nat.lt_of_not_le Hsh))
    rw [hS0] at hk
    have hk0 : k = 0 := Nat.le_antisymm hk (Nat.zero_le k)
    rw [hk0, List.drop_zero, Nat.sub_zero]

/-- **P4: reachability.** Extending the window backward by the
    missing `lo` lines reaches the start of the log. There is no
    hard cap: any line is reachable. -/
theorem P4_reach_start (v : View) (older : List Line) (hol : older.length = v.lo) :
    (extendBack v older).lo = 0 := by
  unfold extendBack
  rw [hol, Nat.sub_self]

/-- Repeated chunked backward extension: after `n` chunks of `c`
    lines, `max 0 (lo - n * c)` lines remain to the log start. -/
theorem chunks_left_eq (lo c n : Nat) (hc : 0 < c) :
    chunksLeft lo c n = max 0 (lo - n * c) := by
  have _ := hc
  induction n with
  | zero =>
    dsimp [chunksLeft]
    rw [Nat.zero_mul, Nat.sub_zero, Nat.max_eq_right (a := 0) (b := lo) (Nat.zero_le lo)]
  | succ m ih =>
    dsimp [chunksLeft]
    rw [ih]
    -- Goal: chunkLo (max 0 (lo - m*c)) c = max 0 (lo - (m+1)*c)
    dsimp [chunkLo]
    have hmul : m * c + c = (m + 1) * c := by
      calc
        m * c + c = c + m * c := Nat.add_comm (m * c) c
        _ = c * m + c := by
          rw [Nat.mul_comm m c]
          exact Nat.add_comm c (c * m)
        _ = c * (m + 1) := (Nat.mul_succ c m).symm
        _ = (m + 1) * c := Nat.mul_comm c (m + 1)
    by_cases hB : lo ≤ m * c
    · -- lo - m*c = 0: no lines remain, and none ever will
      have hrem : lo - m * c = 0 := Nat.sub_eq_zero_of_le hB
      have hmax : max 0 (lo - m * c) = 0 := by
        rw [hrem, Nat.max_eq_left (a := 0) (b := 0) (Nat.le_refl 0)]
      rw [hmax]
      have hmin : min c 0 = 0 := by
        exact Nat.min_eq_right (a := c) (b := 0) (Nat.zero_le c)
      rw [hmin, Nat.sub_self]
      have hlo : lo ≤ (m + 1) * c := by
        have hmid : lo ≤ m * c + c := Nat.le_trans hB (Nat.le_add_right (m * c) c)
        rw [← hmul]
        exact hmid
      have hrem2 : lo - (m + 1) * c = 0 := Nat.sub_eq_zero_of_le hlo
      rw [hrem2, Nat.max_eq_left (a := 0) (b := 0) (Nat.le_refl 0)]
    · -- m*c < lo: lo - m*c is exact
      have hA : m * c < lo := Nat.lt_of_not_le hB
      have hmax : max 0 (lo - m * c) = lo - m * c :=
        Nat.max_eq_right (a := 0) (b := lo - m * c) (Nat.zero_le (lo - m * c))
      rw [hmax]
      by_cases hbig : c ≤ lo - m * c
      · -- min = c: LHS = lo - m*c - c = lo - (m+1)*c
        rw [Nat.min_eq_left hbig, Nat.sub_sub, hmul]
        exact Nat.max_eq_right (a := 0) (b := lo - (m + 1) * c) (Nat.zero_le _)
      · -- lo - m*c < c: the chunk saturates and lo - (m+1)*c = 0
        have hsmall : lo - m * c < c := Nat.lt_of_not_le hbig
        have hmin : min c (lo - m * c) = lo - m * c := by
          exact Nat.min_eq_right (a := c) (b := lo - m * c) (Nat.le_of_lt hsmall)
        rw [hmin, Nat.sub_self]
        have hexact : lo - m * c + m * c = lo := Nat.sub_add_cancel (Nat.le_of_lt hA)
        have hlt : lo - m * c + m * c < m * c + c := by
          have hlt1 : lo - m * c + m * c < c + m * c :=
            Nat.add_lt_add_right hsmall (m * c)
          rw [← Nat.add_comm c (m * c)]
          exact hlt1
        have hlo_lt : lo < m * c + c := by
          rw [← hexact]
          exact hlt
        have hlo2 : lo < (m + 1) * c := by
          rw [← hmul]
          exact hlo_lt
        have hrem : lo - (m + 1) * c = 0 :=
          Nat.sub_eq_zero_of_le (Nat.le_of_lt hlo2)
        rw [hrem, Nat.max_eq_left (a := 0) (b := 0) (Nat.le_refl 0)]

/-- **P4b: chunked reachability.** Chunked backward extension of
    `c` lines at a time reaches the log start after enough chunks:
    when `lo ≤ n * c`, the remaining distance is 0. -/
theorem P4b_chunked_reach (v : View) (c : Nat) (n : Nat)
    (hc : 0 < c) (hn : v.lo ≤ n * c) :
    chunksLeft v.lo c n = 0 := by
  rw [chunks_left_eq v.lo c n hc]
  have hrem : v.lo - n * c = 0 := Nat.sub_eq_zero_of_le hn
  rw [hrem]
  exact Nat.max_eq_left (a := 0) (b := 0) (Nat.le_refl 0)

/-- **P5: bounded window.** While following the tail, evicting down
    to `h + buffer` lines keeps the window bounded: exactly
    `h + buffer` lines remain and tail-following is preserved. -/
theorem P5_bounded (v : View) (buffer : Nat)
    (htail : v.scroll = 0)
    (hlen : v.h + buffer ≤ v.lines.length) :
    let k := v.lines.length - (v.h + buffer)
    (evictFront v k).scroll = 0 ∧
    (evictFront v k).lines.length = v.h + buffer := by
  dsimp only [evictFront]
  constructor
  · simp [htail]
  · rw [List.length_drop]
    -- Goal: L - (L - (h + buffer)) = h + buffer
    exact nat_sub_sub_self v.lines.length (v.h + buffer) hlen

/-! ## Concrete examples (mirror the Rust behavior) -/

/-- A 10-line transcript, viewport h = 4, scrolled up 3 lines.
    The viewport shows lines 3-6 (0-indexed). -/
def exP1_base : View :=
  { lines  := [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
  , lo     := 0
  , scroll := 3
  , h      := 4 }

/-- After appending 2 new lines and compensating scroll,
    the viewport is unchanged. -/
theorem exP1_append :
    visible (onAppend exP1_base [10, 11]) = visible exP1_base := by
  have hp : 0 < exP1_base.scroll := by decide
  have hfit : exP1_base.h ≤ exP1_base.lines.length := by decide
  exact P1_no_flush exP1_base [10, 11] hp hfit

/-- A 10-line transcript at the tail, viewport h = 4. -/
def exP3_base : View :=
  { lines  := [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
  , lo     := 0
  , scroll := 0
  , h      := 4 }

/-- Evicting 2 lines from the front of a 10-line transcript with
    scroll = 0, h = 4: the viewport (lines 6-9) is unchanged. -/
theorem exP3_evict :
    visible (evictFront exP3_base 2) = visible exP3_base := by
  have hk : 2 ≤ startIdx exP3_base := by
    unfold startIdx
    decide
  exact P3_evict_safe exP3_base 2 hk

/-- While following the tail, a batch of 4 new lines (h = 4) fills
    the viewport entirely: the user sees exactly the new lines. -/
theorem exP2b_tail :
    visible (onAppend exP3_base [10, 11, 12, 13]) = [10, 11, 12, 13] := by
  have ht : exP3_base.scroll = 0 := by decide
  have hn : [10, 11, 12, 13].length ≥ exP3_base.h := by decide
  exact P2b_new_tail_visible exP3_base [10, 11, 12, 13] ht hn

/-- Extending backward by `lo` lines always reaches the log start. -/
def exP4_base : View :=
  { lines  := [5, 6, 7, 8, 9]
  , lo     := 5
  , scroll := 0
  , h      := 3 }

theorem exP4_reach :
    (extendBack exP4_base [0, 1, 2, 3, 4]).lo = 0 := by
  have hol : [0, 1, 2, 3, 4].length = exP4_base.lo := by decide
  exact P4_reach_start exP4_base [0, 1, 2, 3, 4] hol

/-- Chunked reachability: from lo = 10, chunks of 3 lines reach the
    log start within 4 chunks (4 * 3 ≥ 10). -/
theorem exP4b_chunks : chunksLeft 10 3 4 = 0 := by
  rw [chunks_left_eq 10 3 4 (show 0 < 3 from by decide)]
  decide

/-- While following the tail, evicting down to `h + 2` lines keeps
    the window bounded. -/
def exP5_base : View :=
  { lines  := [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]
  , lo     := 0
  , scroll := 0
  , h      := 4 }

theorem exP5_bounded :
    let k := exP5_base.lines.length - (exP5_base.h + 2)
    (evictFront exP5_base k).scroll = 0 ∧
    (evictFront exP5_base k).lines.length = exP5_base.h + 2 := by
  have htail : exP5_base.scroll = 0 := by decide
  have hlen : exP5_base.h + 2 ≤ exP5_base.lines.length := by decide
  exact P5_bounded exP5_base 2 htail hlen

end TuiViewportSpec
