/-
TuiStreamSpec — Formal specification for "stream rendering of the model
response."

Source requirement:
  docs/tui_feature_requests_from_human.md, "New requests (2026-09-06)":
  "Stream rendering of the model response."

The TUI currently shows a model response only after the whole
`assistant_message` event lands in the session log. This feature lets the
response render incrementally: as chunks of the response stream in, the
partial text is displayed; when the response completes, the full text is
settled into the transcript. This module specifies that behavior;
the Rust implementation is shipped (`bin/model` produces the delta
lines, `bin/rushi` owns the stream file, `bin/tui` renders the live
block — contract and proofs in docs/tui-streaming-response.md
section "Verification").

The model is a pure reference renderer:

  * a response stream is a sequence of `StreamEvent`s:
      - `StreamEvent.chunk c` : chunk `c` of response text has arrived
      - `StreamEvent.finished` : the response is complete
  * the renderer state `View` holds:
      - `draft`     : the in-progress response text so far
      - `settled`   : completed responses, oldest first
      - `followTail`: whether the viewport is pinned to the tail
      - `scroll`    : when not following, the scroll offset from the tail

The invariants (P1…P6) are stated as theorems and proven. The Lean kernel
re-checks every proof; a clean build with zero `sorry` is the guarantee
that the spec is internally consistent. See docs/lean-driven-development.md
§8.

Differential random testing: `lean/TuiStreamDrt.lean` is a pure CLI
executable over this spec's reference renderer; `bin/tui-stream-drt`
is its Rust mirror. The DRT regression gate runs via `lean-verify`
`op=drt` (docs/tui-streaming-response.md "Gate").
-/



namespace TuiStreamSpec

/-! ## Model -/

/-- A chunk of response text, modelled as a sequence of characters. -/
abbrev Text := List Char

/-- One event of a single model-response stream. -/
inductive StreamEvent
  /-- More response text has arrived. -/
  | chunk (t : Text)
  /-- The response is complete. -/
  | finished

/-- The full text of a response: its chunks joined in the order they were
    streamed. -/
def respText : List Text → Text
  | []       => []
  | c :: rest => c ++ respText rest

/-- Renderer state while a response is in flight, plus the transcript of
    responses that are already settled. -/
structure View where
  /-- Response text accumulated so far for the current stream. -/
  draft : Text
  /-- Completed responses, oldest first. Each entry is the full text of
      one finished response. -/
  settled : List Text
  /-- Viewport choice: pinned to the tail when `true`, a fixed scroll
      position when `false`. Streaming must not disturb this choice. -/
  followTail : Bool
  /-- When `followTail = false`, the lines scrolled up from the tail. -/
  scroll : Nat

/-- The empty transcript, following the tail. -/
def initialView : View :=
  { draft := [], settled := [], followTail := true, scroll := 0 }

/-- Apply one stream event to the view. A chunk only appends to the
    draft; `finished` settles the draft into the transcript. Neither
    touches the viewport fields. -/
def View.step (self : View) (e : StreamEvent) : View :=
  match e with
  | .chunk t => { self with draft := self.draft ++ t }
  | .finished =>
      { self with settled := self.settled ++ [self.draft], draft := [] }

/-- Run a whole response (its chunks in order, then `finished`) starting
    from view `v`. -/
def runResponse (v : View) (r : List Text) : View :=
  match r with
  | []      => v.step StreamEvent.finished
  | c :: rs => runResponse (v.step (StreamEvent.chunk c)) rs

/-- Run a list of responses in order, each settling into the transcript. -/
def runResponses (v : View) (rs : List (List Text)) : View :=
  match rs with
  | []        => v
  | r :: rest => runResponses (runResponse v r) rest

/-! ## Invariants -/

/-- A response text built from a concatenation splits additively over
    the split point. -/
theorem respText_append (left right : List Text) :
    respText (left ++ right) = respText left ++ respText right := by
  induction left with
  | nil =>
      rfl
  | cons c left ih =>
      rw [List.cons_append]
      dsimp [respText]
      rw [ih, List.append_assoc]

/-! ### P1: convergence — the transcript gains exactly the full
    response text -/

/-- After a response completes, the transcript grows by one entry equal
    to the carried draft plus the full response text. Nothing is lost or
    duplicated. -/
theorem P1_converges (v : View) (r : List Text) :
    (runResponse v r).settled = v.settled ++ [v.draft ++ respText r] := by
  induction r generalizing v with
  | nil =>
      simp [runResponse, View.step, respText]
  | cons c rs ih =>
      dsimp [runResponse]
      have hstep := ih (v.step (StreamEvent.chunk c))
      rw [hstep]
      dsimp [View.step, StreamEvent.chunk]
      simp [respText, List.append_assoc]

/-! ### P2: clean settle — no partial text lingers after a response -/

/-- When a response completes, the in-progress draft is cleared; the next
    response starts from an empty draft. -/
theorem P2_clean_after_response (v : View) (r : List Text) :
    (runResponse v r).draft = [] := by
  induction r generalizing v with
  | nil =>
      simp [runResponse, View.step]
  | cons c rs ih =>
      dsimp [runResponse]
      exact ih (v.step (StreamEvent.chunk c))

/-! ### P3: prefix growth — the partial render is always a prefix of the
    full text -/

/-- The text rendered after any prefix `left` of the chunks is a prefix
    of the full response text: some `tail` (namely the text of `right`)
    completes it. -/
theorem P3_prefix (left right : List Text) :
    ∃ tail, respText (left ++ right) = respText left ++ tail := by
  refine ⟨respText right, ?_⟩
  rw [respText_append]

/-! ### P4: no flush — streaming never disturbs the viewport -/

/-- Chunks and the completion event leave the user's viewport choice
    (follow-tail vs. pinned scroll) untouched. -/
theorem P4_no_flush (v : View) (r : List Text) :
    (runResponse v r).followTail = v.followTail ∧
    (runResponse v r).scroll = v.scroll := by
  induction r generalizing v with
  | nil =>
      simp [runResponse, View.step]
  | cons c rs ih =>
      dsimp [runResponse]
      have hf' := (ih (v.step (StreamEvent.chunk c))).1
      have hs' := (ih (v.step (StreamEvent.chunk c))).2
      rw [hf', hs']
      exact ⟨rfl, rfl⟩

/-! ### P5: transcript append-only — settled text is never rewritten -/

/-- The transcript only grows: the old settled list is a prefix of the
    new one. -/
theorem P5_transcript_grows (v : View) (r : List Text) :
    ∃ rest, (runResponse v r).settled = v.settled ++ rest := by
  refine ⟨[v.draft ++ respText r], ?_⟩
  rw [P1_converges]

/-! ### P6: multi-response convergence — responses stream in order -/

/-- Processing a whole sequence of responses (from a clean draft) settles
    each one, in order, into the transcript. -/
theorem P6_multi_converges (v : View) (rs : List (List Text)) (h : v.draft = []) :
    (runResponses v rs).settled = v.settled ++ List.map respText rs := by
  induction rs generalizing v h with
  | nil =>
      simp [runResponses, List.map, List.append_nil]
  | cons r rs ih =>
      dsimp [runResponses]
      have ih' := ih (runResponse v r) (P2_clean_after_response v r)
      rw [ih']
      rw [P1_converges, h, List.nil_append]
      simp [List.append_assoc, List.cons_append]

/-! ## Concrete examples (mirrored in Rust unit tests) -/

/-
Each example below has a Rust mirror, so the kernel-checked spec and
the implementation tests pin the same behavior:

* `ex_stream_hello`, `ex_stream_clean` — producer:
  `sse_parser_emits_channel_lines_as_lines_arrive`
  (`bin/model/src/main.rs`); consumer:
  `refresh_stream_reads_from_byte_zero_on_fresh_app`
  (`bin/tui/src/app.rs`).
* `ex_empty_response` — producer: `sse_parser_empty_response_settles_done_only`;
  consumer: `ex_empty_response_done_only_channel_settles`.
* `ex_two_responses` — consumer: `ex_two_responses_settle_in_order`.
-/

def he   : Text := ['H', 'e']
def llo  : Text := ['l', 'l', 'o']
def hello: Text := ['H', 'e', 'l', 'l', 'o']

/-- Streaming "He" then "llo" settles to the full text "Hello". -/
theorem ex_stream_hello :
    (runResponse initialView [he, llo]).settled = [hello] := by
  dsimp [he, llo, hello]
  decide

/-- After a complete response the draft is cleared. -/
theorem ex_stream_clean :
    (runResponse initialView [he, llo]).draft = [] := by
  dsimp [he, llo]
  decide

/-- An empty response still settles exactly one (empty) entry. -/
theorem ex_empty_response :
    (runResponse initialView []).settled = [[]] := by
  decide

/-- Two responses settle in order. -/
theorem ex_two_responses :
    (runResponses initialView [ [ ['A'] ], [ ['B'] ] ]).settled =
      [ ['A'], ['B'] ] := by
  decide

end TuiStreamSpec